use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use tar::Archive;
use zip::ZipArchive;

use crate::lua_skill::validate_luaskills_identifier;
use crate::runtime::path::render_host_visible_path;
use crate::skill::dependencies::{DependencyArchiveType, DependencyExportSpec};

/// One dependency export paired with its validated destination under the install root.
/// 单个依赖导出及其位于安装根目录下、已经验证的目标路径。
struct ResolvedDependencyExport<'a> {
    /// Original export declaration used to locate the source archive entry and executable flag.
    /// 用于定位源归档条目及可执行标记的原始导出声明。
    spec: &'a DependencyExportSpec,
    /// Validated host path that cannot lexically escape the declared dependency install root.
    /// 已验证且在词法上无法越出声明依赖安装根目录的宿主路径。
    target_path: PathBuf,
}

/// Render one archive filesystem path for user-facing error messages.
/// 为面向用户的归档错误消息渲染单个文件系统路径。
fn render_archive_path(path: &Path) -> String {
    render_host_visible_path(path)
}

/// Inspect whether one extracted skill manifest path is a file without hiding filesystem probe errors.
/// 检查单个已解压技能清单路径是否为文件，同时不隐藏文件系统探测错误。
///
/// The skill_yaml parameter is the concrete skill.yaml path expected inside one extracted skill directory.
/// skill_yaml 参数是单个已解压技能目录内预期存在的具体 skill.yaml 路径。
///
/// Return true for an existing manifest file, false for a confirmed missing manifest file, or an explicit probe/type error.
/// 已存在清单文件返回 true，确认缺失清单文件返回 false；探测或类型异常时返回显式错误。
fn extracted_skill_manifest_is_file(skill_yaml: &Path) -> Result<bool, String> {
    match fs::metadata(skill_yaml) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(format!(
            "Extracted skill manifest is not a file: {}",
            render_archive_path(skill_yaml)
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "Failed to inspect extracted skill manifest {}: {}",
            render_archive_path(skill_yaml),
            error
        )),
    }
}

/// Install one downloaded payload into the dependency root according to export rules.
/// 按导出规则把单个已下载载荷安装到依赖根目录。
pub fn install_downloaded_payload(
    archive_path: &Path,
    archive_type: DependencyArchiveType,
    install_root: &Path,
    exports: &[DependencyExportSpec],
) -> Result<(), String> {
    // ResolvedExports validates every destination before creating directories or copying any payload bytes.
    // ResolvedExports 在创建目录或复制任何载荷字节前一次性验证全部目标路径。
    let resolved_exports = resolve_dependency_export_targets(install_root, exports)?;
    for export in &resolved_exports {
        validate_dependency_export_target_disk_state(install_root, &export.target_path)?;
    }
    fs::create_dir_all(install_root).map_err(|error| {
        format!(
            "Failed to create {}: {}",
            render_archive_path(install_root),
            error
        )
    })?;
    match archive_type {
        DependencyArchiveType::Raw => install_from_raw_file(archive_path, &resolved_exports),
        DependencyArchiveType::Zip => install_from_zip_archive(archive_path, &resolved_exports),
        DependencyArchiveType::TarGz => {
            install_from_tar_gz_archive(archive_path, &resolved_exports)
        }
    }
}

/// Extract one skill package zip into a temporary root and return the extracted skill directory.
/// 把单个技能包 zip 解压到临时根目录，并返回解压得到的技能目录。
pub fn extract_skill_package_zip(
    archive_path: &Path,
    temp_root: &Path,
    expected_skill_id: &str,
) -> Result<PathBuf, String> {
    validate_luaskills_identifier(expected_skill_id, "expected_skill_id")?;
    fs::create_dir_all(temp_root).map_err(|error| {
        format!(
            "Failed to create {}: {}",
            render_archive_path(temp_root),
            error
        )
    })?;
    let file = fs::File::open(archive_path).map_err(|error| {
        format!(
            "Failed to open {}: {}",
            render_archive_path(archive_path),
            error
        )
    })?;
    let mut archive =
        ZipArchive::new(file).map_err(|error| format!("Failed to open zip archive: {}", error))?;

    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("Failed to read zip entry #{}: {}", index, error))?;
        let entry_path = normalize_zip_entry_path(entry.name())?;
        if entry_path.components().next().is_none() {
            continue;
        }

        let top_level = entry_path
            .components()
            .next()
            .and_then(|component| component.as_os_str().to_str())
            .ok_or_else(|| {
                format!(
                    "Failed to read the top-level directory of zip entry '{}'",
                    entry.name()
                )
            })?;
        if top_level != expected_skill_id {
            return Err(format!(
                "Skill package {} must contain only the top-level directory '{}', but found '{}'",
                render_archive_path(archive_path),
                expected_skill_id,
                top_level
            ));
        }

        let target_path = temp_root.join(&entry_path);
        if entry.is_dir() {
            fs::create_dir_all(&target_path).map_err(|error| {
                format!(
                    "Failed to create {}: {}",
                    render_archive_path(&target_path),
                    error
                )
            })?;
            continue;
        }

        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "Failed to create {}: {}",
                    render_archive_path(parent),
                    error
                )
            })?;
        }
        let mut output = fs::File::create(&target_path).map_err(|error| {
            format!(
                "Failed to create {}: {}",
                render_archive_path(&target_path),
                error
            )
        })?;
        std::io::copy(&mut entry, &mut output).map_err(|error| {
            format!(
                "Failed to extract '{}' into {}: {}",
                entry.name(),
                render_archive_path(&target_path),
                error
            )
        })?;
    }

    let skill_dir = temp_root.join(expected_skill_id);
    let skill_yaml = skill_dir.join("skill.yaml");
    if !extracted_skill_manifest_is_file(&skill_yaml)? {
        return Err(format!(
            "Skill package {} does not contain {}/skill.yaml",
            render_archive_path(archive_path),
            expected_skill_id
        ));
    }
    Ok(skill_dir)
}

/// Install exports from one raw single-file payload.
/// 从单个原始文件载荷中安装导出文件。
fn install_from_raw_file(
    archive_path: &Path,
    exports: &[ResolvedDependencyExport<'_>],
) -> Result<(), String> {
    if exports.len() != 1 {
        return Err("raw dependency payload must declare exactly one export".to_string());
    }
    // Export is safe to index after the exact-one declaration check above.
    // Export 在上方恰好一个声明的检查通过后可安全索引。
    let export = &exports[0];
    copy_file_with_parent_dir(archive_path, &export.target_path)?;
    mark_executable_if_needed(&export.target_path, export.spec.executable)?;
    Ok(())
}

/// Install exports from one zip archive payload.
/// 从单个 zip 归档载荷中安装导出文件。
fn install_from_zip_archive(
    archive_path: &Path,
    exports: &[ResolvedDependencyExport<'_>],
) -> Result<(), String> {
    let file = fs::File::open(archive_path).map_err(|error| {
        format!(
            "Failed to open {}: {}",
            render_archive_path(archive_path),
            error
        )
    })?;
    let mut archive =
        ZipArchive::new(file).map_err(|error| format!("Failed to open zip archive: {}", error))?;
    for export in exports {
        let entry_name = resolve_zip_export_entry_name(&mut archive, &export.spec.archive_path)
            .ok_or_else(|| {
                format!(
                    "Failed to read zip entry '{}' from {}: specified file not found in archive",
                    export.spec.archive_path,
                    render_archive_path(archive_path)
                )
            })?;
        let mut entry = archive.by_name(&entry_name).map_err(|error| {
            format!(
                "Failed to read zip entry '{}' from {}: {}",
                entry_name,
                render_archive_path(archive_path),
                error
            )
        })?;
        if let Some(parent) = export.target_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "Failed to create {}: {}",
                    render_archive_path(parent),
                    error
                )
            })?;
        }
        let mut output = fs::File::create(&export.target_path).map_err(|error| {
            format!(
                "Failed to create {}: {}",
                render_archive_path(&export.target_path),
                error
            )
        })?;
        std::io::copy(&mut entry, &mut output).map_err(|error| {
            format!(
                "Failed to extract '{}' into {}: {}",
                export.spec.archive_path,
                render_archive_path(&export.target_path),
                error
            )
        })?;
        mark_executable_if_needed(&export.target_path, export.spec.executable)?;
    }
    Ok(())
}

/// Install exports from one tar.gz archive payload.
/// 从单个 tar.gz 归档载荷中安装导出文件。
fn install_from_tar_gz_archive(
    archive_path: &Path,
    exports: &[ResolvedDependencyExport<'_>],
) -> Result<(), String> {
    // ArchiveFile streams compressed bytes directly from disk instead of retaining the whole archive.
    // ArchiveFile 直接从磁盘流式读取压缩字节，不再保留整个归档。
    let archive_file = fs::File::open(archive_path).map_err(|error| {
        format!(
            "Failed to open {}: {}",
            render_archive_path(archive_path),
            error
        )
    })?;
    // Decoder incrementally expands the file into the tar reader.
    // Decoder 将文件增量解压到 tar 读取器。
    let decoder = GzDecoder::new(archive_file);
    let mut archive = Archive::new(decoder);
    let mut extracted_entries: Vec<(PathBuf, bool)> = Vec::new();
    // MatchedExports records only entries copied from this archive, never stale files already on disk.
    // MatchedExports 只记录从本次归档复制的条目，绝不把磁盘上的陈旧文件算作命中。
    let mut matched_exports = vec![false; exports.len()];

    for archive_entry in archive.entries().map_err(|error| {
        format!(
            "Failed to enumerate tar.gz entries from {}: {}",
            render_archive_path(archive_path),
            error
        )
    })? {
        let mut archive_entry =
            archive_entry.map_err(|error| format!("Failed to read tar entry: {}", error))?;
        let entry_path = archive_entry
            .path()
            .map_err(|error| format!("Failed to read tar entry path: {}", error))?
            .into_owned();
        let entry_path = normalize_tar_entry_match_path(&entry_path)?;
        // MatchingExportIndices preserves every declaration that consumes the same source entry.
        // MatchingExportIndices 保留消费同一源条目的全部导出声明。
        let matching_export_indices = exports
            .iter()
            .enumerate()
            .filter_map(|(index, export)| {
                archive_entry_matches_export(&entry_path, &export.spec.archive_path)
                    .then_some(index)
            })
            .collect::<Vec<_>>();
        let Some((&first_export_index, remaining_export_indices)) =
            matching_export_indices.split_first()
        else {
            continue;
        };
        // FirstExport receives the archive stream directly so entry size does not affect memory usage.
        // FirstExport 直接接收归档流，因此条目大小不会影响内存占用。
        let first_export = &exports[first_export_index];
        if let Some(parent) = first_export.target_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "Failed to create {}: {}",
                    render_archive_path(parent),
                    error
                )
            })?;
        }
        let mut output = fs::File::create(&first_export.target_path).map_err(|error| {
            format!(
                "Failed to create {}: {}",
                render_archive_path(&first_export.target_path),
                error
            )
        })?;
        std::io::copy(&mut archive_entry, &mut output).map_err(|error| {
            format!(
                "Failed to extract '{}' from {}: {}",
                first_export.spec.archive_path,
                render_archive_path(archive_path),
                error
            )
        })?;
        // Output is closed before the first target becomes the source for any additional copies.
        // Output 会在首个目标作为后续复制源之前关闭。
        drop(output);
        matched_exports[first_export_index] = true;
        extracted_entries.push((
            first_export.target_path.clone(),
            first_export.spec.executable,
        ));
        for &export_index in remaining_export_indices {
            // Export reuses the already streamed first target without retaining entry bytes in memory.
            // Export 复用已经流式写入的首个目标，不在内存中保留条目字节。
            let export = &exports[export_index];
            if export.target_path != first_export.target_path {
                copy_file_with_parent_dir(&first_export.target_path, &export.target_path)?;
            }
            matched_exports[export_index] = true;
            extracted_entries.push((export.target_path.clone(), export.spec.executable));
        }
    }

    for (export, matched) in exports.iter().zip(matched_exports) {
        if !matched {
            return Err(format!(
                "tar.gz archive {} does not contain required export '{}'",
                render_archive_path(archive_path),
                export.spec.archive_path
            ));
        }
    }

    for (target_path, executable) in extracted_entries {
        mark_executable_if_needed(&target_path, executable)?;
    }
    Ok(())
}

/// Resolve one zip export entry by exact path first and then by stripping one leading archive directory.
/// 先按精确路径解析单个 zip 导出条目，再尝试剥离一层归档顶层目录进行匹配。
fn resolve_zip_export_entry_name<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    expected_archive_path: &str,
) -> Option<String> {
    let expected = normalize_archive_entry_match_path(expected_archive_path);
    if let Some(entry_name) = archive
        .file_names()
        .find(|name| normalize_archive_entry_match_path(name) == expected)
    {
        return Some(entry_name.to_string());
    }

    archive.file_names().find_map(|name| {
        let normalized_name = normalize_archive_entry_match_path(name);
        if strip_one_leading_archive_component(&normalized_name).as_deref()
            == Some(expected.as_str())
        {
            Some(name.to_string())
        } else {
            None
        }
    })
}

/// Return whether one archive entry matches one declared export path directly or after stripping one top-level directory.
/// 判断单个归档条目是否能与声明的导出路径直接匹配，或在剥离一层顶层目录后匹配。
fn archive_entry_matches_export(entry_path: &str, export_archive_path: &str) -> bool {
    let normalized_entry = normalize_archive_entry_match_path(entry_path);
    let normalized_export = normalize_archive_entry_match_path(export_archive_path);
    normalized_entry == normalized_export
        || strip_one_leading_archive_component(&normalized_entry).as_deref()
            == Some(normalized_export.as_str())
}

/// Normalize one archive entry path into a stable forward-slash matching representation.
/// 把单个归档条目路径规范化为稳定的正斜杠匹配表示。
fn normalize_archive_entry_match_path(raw: &str) -> String {
    raw.replace('\\', "/").trim_matches('/').to_string()
}

/// Normalize one tar entry path into the shared archive export matching representation.
/// 将单个 tar 条目路径规范化为共享的归档导出匹配表示。
///
/// The entry_path parameter is the path reported by the tar archive entry header.
/// entry_path 参数是 tar 归档条目头报告的路径。
///
/// Return the normalized UTF-8 archive path used to match dependency export declarations.
/// 返回用于匹配依赖导出声明的 UTF-8 归档路径。
fn normalize_tar_entry_match_path(entry_path: &Path) -> Result<String, String> {
    let entry_text = entry_path
        .to_str()
        .ok_or_else(|| "tar.gz entry path must be valid UTF-8".to_string())?;
    Ok(normalize_archive_entry_match_path(entry_text))
}

/// Strip exactly one leading path component from one normalized archive entry path.
/// 从一个已规范化的归档条目路径中剥离恰好一层顶层路径片段。
fn strip_one_leading_archive_component(normalized_path: &str) -> Option<String> {
    let mut components = normalized_path
        .split('/')
        .filter(|component| !component.is_empty());
    components.next()?;
    let remainder = components.collect::<Vec<_>>();
    if remainder.is_empty() {
        None
    } else {
        Some(remainder.join("/"))
    }
}

/// Resolve one portable dependency export target while rejecting every lexical root-escape form.
/// 解析单个可移植依赖导出目标，同时拒绝全部词法层面的根目录逃逸形式。
///
/// The root parameter is the host-owned dependency install root.
/// root 参数是宿主管理的依赖安装根目录。
///
/// The relative_target parameter is the slash- or backslash-separated manifest destination.
/// relative_target 参数是清单中使用正斜杠或反斜杠分隔的目标路径。
///
/// Return the target joined under root, or an error for empty, absolute, drive-prefixed, or dot-component paths.
/// 返回拼接在 root 下的目标路径；空路径、绝对路径、驱动器前缀或点路径片段会返回错误。
pub(crate) fn resolve_dependency_export_target(
    root: &Path,
    relative_target: &str,
) -> Result<PathBuf, String> {
    // PortableTarget makes both manifest separator styles obey the same validation on every host OS.
    // PortableTarget 让清单中的两种分隔符在所有宿主操作系统上接受同一套验证。
    let portable_target = relative_target.replace('\\', "/");
    // Bytes are used only for the ASCII Windows drive-prefix grammar.
    // Bytes 仅用于识别 ASCII Windows 驱动器前缀语法。
    let target_bytes = portable_target.as_bytes();
    // HasWindowsDrivePrefix rejects both drive-absolute and drive-relative paths before host joining.
    // HasWindowsDrivePrefix 在宿主路径拼接前同时拒绝驱动器绝对路径与驱动器相对路径。
    let has_windows_drive_prefix =
        target_bytes.len() >= 2 && target_bytes[0].is_ascii_alphabetic() && target_bytes[1] == b':';
    if portable_target.is_empty() || portable_target.starts_with('/') || has_windows_drive_prefix {
        return Err(format!(
            "Dependency export target '{}' must be a non-empty relative path without a root or drive prefix",
            relative_target
        ));
    }

    // ResolvedTarget is built from individually accepted normal components, never from an unchecked path.
    // ResolvedTarget 仅由逐个通过检查的普通路径片段构造，绝不直接拼接未经检查的路径。
    let mut resolved_target = root.to_path_buf();
    for component in portable_target.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(format!(
                "Dependency export target '{}' must contain only non-empty normal path components",
                relative_target
            ));
        }
        resolved_target.push(component);
    }
    Ok(resolved_target)
}

/// Resolve every dependency export destination once before installation starts.
/// 在安装开始前一次性解析全部依赖导出目标。
///
/// The root parameter is the dependency install root shared by all declarations.
/// root 参数是全部声明共享的依赖安装根目录。
///
/// The exports parameter contains the source-entry and destination declarations to resolve.
/// exports 参数包含需要解析的源条目与目标声明。
///
/// Return declarations paired with validated targets, preserving declaration order.
/// 返回按声明顺序排列、与已验证目标配对的声明。
fn resolve_dependency_export_targets<'a>(
    root: &Path,
    exports: &'a [DependencyExportSpec],
) -> Result<Vec<ResolvedDependencyExport<'a>>, String> {
    exports
        .iter()
        .map(|spec| {
            resolve_dependency_export_target(root, &spec.target_path)
                .map(|target_path| ResolvedDependencyExport { spec, target_path })
        })
        .collect()
}

/// Validate existing target components so dependency installation never follows a pre-existing link.
/// 验证现有目标路径片段，确保依赖安装绝不会跟随预先存在的链接。
///
/// The root parameter is the lexical dependency root used by target resolution.
/// root 参数是目标解析所使用的词法依赖根目录。
///
/// The target parameter is one previously resolved destination beneath root.
/// target 参数是一个此前已经解析到 root 下的目标路径。
///
/// Return success for missing components and safe directory/file types, or an explicit unsafe-state error.
/// 路径片段缺失或目录/文件类型安全时返回成功，否则返回明确的不安全状态错误。
fn validate_dependency_export_target_disk_state(root: &Path, target: &Path) -> Result<(), String> {
    // RelativeTarget is guaranteed by the shared lexical resolver and keeps traversal anchored at root.
    // RelativeTarget 由共享词法解析器保证，并使遍历固定在 root 下。
    let relative_target = target.strip_prefix(root).map_err(|error| {
        format!(
            "Dependency export target {} is not beneath install root {}: {}",
            render_archive_path(target),
            render_archive_path(root),
            error
        )
    })?;
    // CurrentPath advances one accepted normal component at a time.
    // CurrentPath 每次前进一个已经接受的普通路径片段。
    let mut current_path = root.to_path_buf();
    // ComponentCount distinguishes the leaf from parent directories without probing twice.
    // ComponentCount 无需重复探测即可区分叶子与父目录。
    let component_count = relative_target.components().count();
    for (index, component) in relative_target.components().enumerate() {
        current_path.push(component.as_os_str());
        // IsLeaf identifies the final export file component.
        // IsLeaf 标识最终导出文件片段。
        let is_leaf = index + 1 == component_count;
        match fs::symlink_metadata(&current_path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "Dependency export target must not traverse or replace a symbolic link: {}",
                    render_archive_path(&current_path)
                ));
            }
            Ok(metadata) if is_leaf && !metadata.is_file() => {
                return Err(format!(
                    "Dependency export target leaf is not a regular file: {}",
                    render_archive_path(&current_path)
                ));
            }
            Ok(metadata) if !is_leaf && !metadata.is_dir() => {
                return Err(format!(
                    "Dependency export target parent is not a directory: {}",
                    render_archive_path(&current_path)
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "Failed to inspect dependency export target component {}: {}",
                    render_archive_path(&current_path),
                    error
                ));
            }
        }
    }
    Ok(())
}

/// Copy one file into the target path and create parent directories first.
/// 将单个文件复制到目标路径，并在复制前创建父级目录。
fn copy_file_with_parent_dir(source: &Path, target: &Path) -> Result<(), String> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "Failed to create {}: {}",
                render_archive_path(parent),
                error
            )
        })?;
    }
    fs::copy(source, target).map_err(|error| {
        format!(
            "Failed to copy {} to {}: {}",
            render_archive_path(source),
            render_archive_path(target),
            error
        )
    })?;
    Ok(())
}

/// Mark one target file executable on Unix platforms when requested.
/// 在需要时把单个目标文件在 Unix 平台上标记为可执行。
fn mark_executable_if_needed(_target: &Path, executable: bool) -> Result<(), String> {
    if !executable {
        return Ok(());
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(_target)
            .map_err(|error| format!("Failed to stat {}: {}", render_archive_path(_target), error))?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(_target, permissions).map_err(|error| {
            format!(
                "Failed to chmod {}: {}",
                render_archive_path(_target),
                error
            )
        })?;
    }

    Ok(())
}

/// Normalize one zip entry path and reject traversal or absolute-path entries.
/// 规范化单个 zip 条目路径，并拒绝目录穿越或绝对路径条目。
fn normalize_zip_entry_path(entry_name: &str) -> Result<PathBuf, String> {
    let normalized = entry_name.replace('\\', "/");
    let mut path = PathBuf::new();
    for component in Path::new(&normalized).components() {
        match component {
            std::path::Component::Normal(value) => path.push(value),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                return Err(format!(
                    "Zip entry '{}' must not contain parent-directory traversal",
                    entry_name
                ));
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return Err(format!(
                    "Zip entry '{}' must not use an absolute path",
                    entry_name
                ));
            }
        }
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::path::render_host_visible_path;

    /// Create one file symbolic link when the current test platform and privileges permit it.
    /// 在当前测试平台与权限允许时创建一个文件符号链接。
    #[cfg(unix)]
    fn create_archive_test_file_symlink(link_path: &Path, target_path: &Path) -> bool {
        std::os::unix::fs::symlink(target_path, link_path).is_ok()
    }

    /// Create one file symbolic link when the current Windows policy permits it.
    /// 在当前 Windows 策略允许时创建一个文件符号链接。
    #[cfg(windows)]
    fn create_archive_test_file_symlink(link_path: &Path, target_path: &Path) -> bool {
        std::os::windows::fs::symlink_file(target_path, link_path).is_ok()
    }

    /// Report unsupported symbolic-link creation on other host families.
    /// 在其他宿主平台族上报告不支持创建符号链接。
    #[cfg(not(any(unix, windows)))]
    fn create_archive_test_file_symlink(_link_path: &Path, _target_path: &Path) -> bool {
        false
    }

    /// Verify tar entry paths reuse the same forward-slash matching representation as zip exports.
    /// 验证 tar 条目路径复用与 zip 导出一致的正斜杠匹配表示。
    #[test]
    fn normalize_tar_entry_match_path_uses_utf8_forward_slash_text() {
        // Archive entry path that uses Windows separators inside the archive name.
        // 在归档名称中使用 Windows 分隔符的归档条目路径。
        let entry_path = Path::new(r"package\bin\demo.exe");
        // Normalized export matching path returned by the tar-specific boundary.
        // tar 专属边界返回的归一化导出匹配路径。
        let normalized =
            normalize_tar_entry_match_path(entry_path).expect("tar entry path should normalize");

        assert_eq!(normalized, "package/bin/demo.exe");
    }

    /// Verify dependency export destinations accept portable normal paths and stay under the install root.
    /// 验证依赖导出目标接受可移植普通路径，并始终位于安装根目录下。
    #[test]
    fn dependency_export_target_resolution_accepts_portable_normal_paths() {
        // InstallRoot is a lexical fixture; target resolution does not need the directory to exist.
        // InstallRoot 是词法路径夹具；目标解析不要求该目录已经存在。
        let install_root = Path::new("dependency-root");

        // ForwardSlashTarget represents the canonical manifest spelling.
        // ForwardSlashTarget 表示清单中的标准写法。
        let forward_slash_target = resolve_dependency_export_target(install_root, "bin/tool.exe")
            .expect("forward-slash target should resolve");
        // BackslashTarget proves Windows-style declarations receive the same portable treatment.
        // BackslashTarget 证明 Windows 风格声明会接受相同的可移植处理。
        let backslash_target = resolve_dependency_export_target(install_root, r"bin\tool.exe")
            .expect("backslash target should resolve");
        // ExpectedTarget is the host-native path assembled from accepted normal components.
        // ExpectedTarget 是由通过检查的普通片段组装出的宿主原生路径。
        let expected_target = install_root.join("bin").join("tool.exe");

        assert_eq!(forward_slash_target, expected_target);
        assert_eq!(backslash_target, expected_target);
    }

    /// Verify every portable absolute, drive-prefixed, empty, and traversal form is rejected.
    /// 验证全部可移植绝对路径、驱动器前缀、空路径及穿越形式都会被拒绝。
    #[test]
    fn dependency_export_target_resolution_rejects_root_escape_forms() {
        // InstallRoot is the boundary that all tested declarations must remain beneath.
        // InstallRoot 是所有被测声明都必须位于其下的边界。
        let install_root = Path::new("dependency-root");
        // InvalidTargets cover both separator styles and host-independent Windows path grammars.
        // InvalidTargets 覆盖两种分隔符及与宿主无关的 Windows 路径语法。
        let invalid_targets = [
            "",
            "../escape.bin",
            r"..\escape.bin",
            "/absolute.bin",
            r"\absolute.bin",
            "C:/absolute.bin",
            r"C:\absolute.bin",
            "C:drive-relative.bin",
            "./relative.bin",
            "bin/../escape.bin",
            "bin//tool.exe",
        ];

        for invalid_target in invalid_targets {
            // Error proves the shared boundary rejects this declaration before any filesystem use.
            // Error 证明共享边界会在任何文件系统操作前拒绝当前声明。
            let error = resolve_dependency_export_target(install_root, invalid_target)
                .expect_err("root-escape form should fail");
            assert!(
                error.contains("Dependency export target"),
                "unexpected error for {invalid_target:?}: {error}"
            );
        }
    }

    /// Verify ZIP export matching preserves the real entry name after portable normalization.
    /// 验证 ZIP 导出匹配在可移植规范化后仍保留真实条目名称。
    #[test]
    fn zip_export_install_uses_original_backslash_entry_name() {
        // TempRoot isolates the generated archive and installation output.
        // TempRoot 隔离生成的归档与安装输出。
        let temp_root = std::env::temp_dir().join(format!(
            "luaskills_archive_zip_entry_name_test_{}",
            std::process::id()
        ));
        if temp_root.exists() {
            // Stale fixture cleanup result is intentionally ignored before recreation.
            // 重建前对陈旧夹具的清理结果有意忽略。
            let _ = std::fs::remove_dir_all(&temp_root);
        }
        std::fs::create_dir_all(&temp_root).expect("temp root should be created");
        // ArchivePath receives one entry with Windows separators and one top-level package directory.
        // ArchivePath 接收一个使用 Windows 分隔符且带单层包目录的条目。
        let archive_path = temp_root.join("payload.zip");
        // ArchiveFile backs the ZIP writer used to preserve the exact entry spelling.
        // ArchiveFile 承载用于保留精确条目写法的 ZIP 写入器。
        let archive_file =
            std::fs::File::create(&archive_path).expect("zip file should be created");
        // ZipWriter emits the concrete backslash entry name exercised by the resolver.
        // ZipWriter 写入解析器本次测试所用的具体反斜杠条目名称。
        let mut zip_writer = zip::ZipWriter::new(archive_file);
        zip_writer
            .start_file(
                r"package\bin\demo.exe",
                zip::write::SimpleFileOptions::default(),
            )
            .expect("zip entry should start");
        std::io::Write::write_all(&mut zip_writer, b"portable payload")
            .expect("zip entry body should be written");
        zip_writer.finish().expect("zip archive should finish");
        // InstallRoot is the host-owned destination for the declared export.
        // InstallRoot 是声明导出的宿主管理目标根目录。
        let install_root = temp_root.join("install");
        // Exports requests the same entry with canonical separators and without the package prefix.
        // Exports 使用标准分隔符且省略包前缀来请求同一条目。
        let exports = vec![DependencyExportSpec {
            archive_path: "bin/demo.exe".to_string(),
            target_path: "bin/demo.exe".to_string(),
            executable: false,
        }];

        install_downloaded_payload(
            &archive_path,
            DependencyArchiveType::Zip,
            &install_root,
            &exports,
        )
        .expect("backslash zip entry should install");

        assert_eq!(
            std::fs::read(install_root.join("bin").join("demo.exe"))
                .expect("installed export should be readable"),
            b"portable payload"
        );
        // Cleanup result is intentionally ignored for best-effort temporary test artifacts.
        // 对临时测试产物的清理结果按最佳努力原则有意忽略。
        let _ = std::fs::remove_dir_all(&temp_root);
    }

    /// Verify invalid raw-export destinations are rejected before any directory or external file is created.
    /// 验证非法原始导出目标会在创建任何目录或外部文件之前被拒绝。
    #[test]
    fn invalid_dependency_export_target_has_no_install_side_effect() {
        // TempRoot isolates both the declared install root and the would-be escaped file.
        // TempRoot 同时隔离声明的安装根目录与原本可能被越界写入的文件。
        let temp_root = std::env::temp_dir().join(format!(
            "luaskills_archive_target_escape_test_{}",
            std::process::id()
        ));
        if temp_root.exists() {
            // Stale fixture cleanup result is intentionally ignored before recreation.
            // 重建前对陈旧夹具的清理结果有意忽略。
            let _ = std::fs::remove_dir_all(&temp_root);
        }
        std::fs::create_dir_all(&temp_root).expect("temp root should be created");
        // ArchivePath is the valid raw payload that must never be copied for an invalid destination.
        // ArchivePath 是有效原始载荷；目标非法时不得复制它。
        let archive_path = temp_root.join("payload.bin");
        std::fs::write(&archive_path, b"payload").expect("raw payload should be written");
        // InstallRoot deliberately does not exist so preflight ordering is observable.
        // InstallRoot 有意保持不存在，以便观察预检顺序。
        let install_root = temp_root.join("install");
        // EscapedTarget is where the prior unchecked join would have written the payload.
        // EscapedTarget 是旧有未检查拼接原本会写入载荷的位置。
        let escaped_target = temp_root.join("escaped.bin");
        // Exports contains the concrete parent-directory traversal declaration under test.
        // Exports 包含本次测试的具体父目录穿越声明。
        let exports = vec![DependencyExportSpec {
            archive_path: "payload.bin".to_string(),
            target_path: "../escaped.bin".to_string(),
            executable: false,
        }];

        // Error comes from global destination validation before install-root creation.
        // Error 来自在安装根目录创建前执行的全局目标验证。
        let error = install_downloaded_payload(
            &archive_path,
            DependencyArchiveType::Raw,
            &install_root,
            &exports,
        )
        .expect_err("traversal destination should fail");

        assert!(error.contains("Dependency export target"), "{error}");
        assert!(!install_root.exists(), "install root must not be created");
        assert!(!escaped_target.exists(), "escaped file must not be created");
        // Cleanup result is intentionally ignored for best-effort temporary test artifacts.
        // 对临时测试产物的清理结果按最佳努力原则有意忽略。
        let _ = std::fs::remove_dir_all(&temp_root);
    }

    /// Verify dependency installation rejects a pre-existing target link without modifying its referent.
    /// 验证依赖安装会拒绝预先存在的目标链接，并且不会修改其指向文件。
    #[test]
    fn dependency_install_rejects_preexisting_target_symlink() {
        // TempRoot isolates the payload, install tree, and external link referent.
        // TempRoot 隔离载荷、安装目录树与链接指向的外部文件。
        let temp_root = std::env::temp_dir().join(format!(
            "luaskills_archive_target_symlink_test_{}",
            std::process::id()
        ));
        if temp_root.exists() {
            // Stale fixture cleanup result is intentionally ignored before recreation.
            // 重建前对陈旧夹具的清理结果有意忽略。
            let _ = std::fs::remove_dir_all(&temp_root);
        }
        // InstallParent is the safe real directory that contains the malicious leaf link.
        // InstallParent 是包含恶意叶子链接的安全真实目录。
        let install_parent = temp_root.join("install").join("bin");
        std::fs::create_dir_all(&install_parent).expect("install parent should be created");
        // OutsideTarget must remain byte-for-byte unchanged after rejection.
        // OutsideTarget 在拒绝后必须逐字节保持不变。
        let outside_target = temp_root.join("outside.bin");
        std::fs::write(&outside_target, b"outside original")
            .expect("outside target should be written");
        // LinkedTarget is the declared export leaf that redirects outside the dependency root.
        // LinkedTarget 是把声明导出重定向到依赖根目录外的叶子链接。
        let linked_target = install_parent.join("tool.bin");
        if !create_archive_test_file_symlink(&linked_target, &outside_target) {
            // Cleanup result is intentionally ignored when host policy cannot create test links.
            // 宿主策略无法创建测试链接时，清理结果有意忽略。
            let _ = std::fs::remove_dir_all(&temp_root);
            return;
        }
        // ArchivePath is the raw payload that the old unchecked copy could write through the link.
        // ArchivePath 是旧有未检查复制可能通过链接写入的原始载荷。
        let archive_path = temp_root.join("payload.bin");
        std::fs::write(&archive_path, b"replacement")
            .expect("raw dependency payload should be written");
        // Exports targets the concrete link path under the lexical install root.
        // Exports 指向词法安装根目录下的具体链接路径。
        let exports = vec![DependencyExportSpec {
            archive_path: "payload.bin".to_string(),
            target_path: "bin/tool.bin".to_string(),
            executable: false,
        }];

        // Error must occur during global disk-state validation before payload copying.
        // Error 必须在载荷复制前的全局磁盘状态验证阶段发生。
        let error = install_downloaded_payload(
            &archive_path,
            DependencyArchiveType::Raw,
            &temp_root.join("install"),
            &exports,
        )
        .expect_err("symbolic-link export target should fail");

        assert!(error.contains("symbolic link"), "unexpected error: {error}");
        assert_eq!(
            std::fs::read(&outside_target).expect("outside target should remain readable"),
            b"outside original"
        );
        // Cleanup result is intentionally ignored for best-effort temporary test artifacts.
        // 对临时测试产物的清理结果按最佳努力原则有意忽略。
        let _ = std::fs::remove_dir_all(&temp_root);
    }

    /// Verify the public skill-package extractor rejects an escaping skill id before touching disk.
    /// 验证公开技能包解压入口会在接触磁盘前拒绝可逃逸的技能标识符。
    #[test]
    fn skill_package_extractor_rejects_traversal_expected_skill_id() {
        // TempRoot isolates the empty archive, extraction root, and pre-existing outside manifest.
        // TempRoot 隔离空归档、解压根目录与预先存在的外部清单。
        let temp_root = std::env::temp_dir().join(format!(
            "luaskills_archive_skill_id_escape_test_{}",
            std::process::id()
        ));
        if temp_root.exists() {
            // Stale fixture cleanup result is intentionally ignored before recreation.
            // 重建前对陈旧夹具的清理结果有意忽略。
            let _ = std::fs::remove_dir_all(&temp_root);
        }
        std::fs::create_dir_all(&temp_root).expect("temp root should be created");
        // ArchivePath contains a valid empty ZIP, which previously reached the unchecked final join.
        // ArchivePath 包含有效空 ZIP；旧逻辑会由此到达未经检查的最终拼接。
        let archive_path = temp_root.join("empty.zip");
        // ArchiveFile receives the empty ZIP central directory.
        // ArchiveFile 用于接收空 ZIP 的中央目录。
        let archive_file =
            std::fs::File::create(&archive_path).expect("empty zip file should be created");
        // ZipWriter finalizes the valid empty archive without adding entries.
        // ZipWriter 在不添加条目的情况下完成有效空归档。
        zip::ZipWriter::new(archive_file)
            .finish()
            .expect("empty zip should finish");
        // ExtractionRoot must remain absent when identifier validation fails.
        // ExtractionRoot 在标识符校验失败时必须保持不存在。
        let extraction_root = temp_root.join("extract");
        // OutsideManifest reproduces the pre-existing file that the old final probe could accept.
        // OutsideManifest 复现旧有最终探测可能接受的预先存在文件。
        let outside_manifest = temp_root.join("outside").join("skill.yaml");
        std::fs::create_dir_all(
            outside_manifest
                .parent()
                .expect("outside manifest should have a parent"),
        )
        .expect("outside directory should be created");
        std::fs::write(&outside_manifest, b"name: outside")
            .expect("outside manifest should be written");

        // Error is returned by the public extractor itself, independent of manager call-site checks.
        // Error 由公开解压入口自身返回，不依赖管理器调用点校验。
        let error = extract_skill_package_zip(&archive_path, &extraction_root, "../outside")
            .expect_err("traversal skill id should fail");

        assert!(error.contains("expected_skill_id must match"), "{error}");
        assert!(
            !extraction_root.exists(),
            "extraction root must not be created"
        );
        assert!(
            outside_manifest.is_file(),
            "outside fixture must remain unchanged"
        );
        // Cleanup result is intentionally ignored for best-effort temporary test artifacts.
        // 对临时测试产物的清理结果按最佳努力原则有意忽略。
        let _ = std::fs::remove_dir_all(&temp_root);
    }

    /// Verify install-root creation errors render paths through the host-visible formatter.
    /// 验证安装根目录创建错误会通过宿主可见路径渲染器输出路径。
    #[test]
    fn install_root_create_error_uses_host_visible_path() {
        // Temporary root that isolates the archive install-root fixture.
        // 隔离归档安装根目录夹具的临时根目录。
        let temp_root = std::env::temp_dir().join(format!(
            "luaskills_archive_install_root_test_{}",
            std::process::id()
        ));
        if temp_root.exists() {
            // Stale fixture cleanup result is intentionally ignored before recreation.
            // 重建前对陈旧夹具的清理结果有意忽略。
            let _ = std::fs::remove_dir_all(&temp_root);
        }
        std::fs::create_dir_all(&temp_root).expect("temp root should be created");
        // File path intentionally reused as install root so create_dir_all fails.
        // 有意复用为安装根目录的文件路径，用于触发 create_dir_all 失败。
        let install_root = temp_root.join("install-root-file");
        std::fs::write(&install_root, b"not a directory")
            .expect("install-root fixture file should be written");
        // Archive path is not reached because install-root creation fails first.
        // 由于安装根目录创建会先失败，该归档路径不会被访问。
        let archive_path = temp_root.join("payload.bin");
        // Error returned by the real archive install entrypoint.
        // 真实归档安装入口返回的错误。
        let error = install_downloaded_payload(
            &archive_path,
            DependencyArchiveType::Raw,
            &install_root,
            &[],
        )
        .expect_err("file install root should fail");
        // Expected diagnostic prefix rendered with the shared host-visible path formatter.
        // 使用共享宿主可见路径渲染器生成的期望诊断前缀。
        let expected_prefix = format!(
            "Failed to create {}:",
            render_host_visible_path(&install_root)
        );

        assert!(
            error.starts_with(&expected_prefix),
            "unexpected error: {}",
            error
        );
        // Cleanup result is intentionally ignored for best-effort temporary test artifacts.
        // 对临时测试产物的清理结果按最佳努力原则有意忽略。
        let _ = std::fs::remove_dir_all(&temp_root);
    }

    /// Verify extracted skill manifest probes report invalid path errors.
    /// 验证已解压技能清单探测会报告非法路径错误。
    #[test]
    fn extracted_skill_manifest_probe_errors_are_reported() {
        // Skill manifest path containing one embedded NUL that metadata cannot inspect.
        // 包含内嵌 NUL 且元数据无法探测的技能清单路径。
        let invalid_skill_yaml = PathBuf::from("invalid\0skill.yaml");

        // Error returned before the invalid manifest can behave like a missing manifest.
        // 在非法清单表现得像缺失清单之前返回的错误。
        let error = extracted_skill_manifest_is_file(&invalid_skill_yaml)
            .expect_err("invalid extracted skill manifest probe should fail");

        assert!(
            error.contains("Failed to inspect extracted skill manifest"),
            "unexpected error: {}",
            error
        );
        assert!(error.contains("invalid"), "unexpected error: {}", error);
    }

    /// Verify extracted skill manifest probes reject directory manifests before later YAML reads.
    /// 验证已解压技能清单探测会在后续 YAML 读取前拒绝目录型清单。
    #[test]
    fn extracted_skill_manifest_rejects_directory_manifest() {
        // Temporary root that isolates the directory manifest fixture.
        // 隔离目录型清单夹具的临时根目录。
        let temp_root = std::env::temp_dir().join(format!(
            "luaskills_archive_directory_manifest_test_{}",
            std::process::id()
        ));
        if temp_root.exists() {
            // Stale fixture cleanup result is intentionally ignored before recreation.
            // 重建前对陈旧夹具的清理结果有意忽略。
            let _ = std::fs::remove_dir_all(&temp_root);
        }
        // Manifest path that exists but is a directory instead of the required YAML file.
        // 已存在但为目录而非所需 YAML 文件的清单路径。
        let skill_yaml = temp_root.join("vulcan-codekit").join("skill.yaml");
        std::fs::create_dir_all(&skill_yaml).expect("directory manifest should be created");

        // Error returned before a directory manifest can behave like a readable YAML file.
        // 在目录型清单表现得像可读取 YAML 文件之前返回的错误。
        let error = extracted_skill_manifest_is_file(&skill_yaml)
            .expect_err("directory extracted skill manifest should fail");

        assert!(
            error.contains("Extracted skill manifest is not a file"),
            "unexpected error: {}",
            error
        );
        assert!(
            error.contains(&render_host_visible_path(&skill_yaml)),
            "unexpected error: {}",
            error
        );
        // Cleanup result is intentionally ignored for best-effort temporary test artifacts.
        // 对临时测试产物的清理结果按最佳努力原则有意忽略。
        let _ = std::fs::remove_dir_all(&temp_root);
    }

    /// Verify tar.gz install never accepts a stale file for an entry missing from the current archive.
    /// 验证 tar.gz 安装绝不会用陈旧文件满足当前归档缺失的条目。
    #[test]
    fn tar_gz_install_rejects_preexisting_file_when_export_missing() {
        // Temporary root that isolates the tar.gz export target fixture.
        // 隔离 tar.gz 导出目标夹具的临时根目录。
        let temp_root = std::env::temp_dir().join(format!(
            "luaskills_archive_stale_export_target_test_{}",
            std::process::id()
        ));
        if temp_root.exists() {
            // Stale fixture cleanup result is intentionally ignored before recreation.
            // 重建前对陈旧夹具的清理结果有意忽略。
            let _ = std::fs::remove_dir_all(&temp_root);
        }
        std::fs::create_dir_all(&temp_root).expect("temp root should be created");
        // Archive path containing a valid tar.gz payload that does not include the declared export.
        // 包含有效 tar.gz 载荷但不包含声明导出的归档路径。
        let archive_path = temp_root.join("payload.tar.gz");
        // Archive file used by the gzip encoder and tar builder.
        // gzip 编码器与 tar 构建器使用的归档文件。
        let archive_file =
            std::fs::File::create(&archive_path).expect("tar.gz archive file should be created");
        // Gzip encoder that wraps the tar archive file.
        // 包装 tar 归档文件的 gzip 编码器。
        let encoder = flate2::write::GzEncoder::new(archive_file, flate2::Compression::default());
        // Tar builder that writes one unrelated file entry.
        // 写入一个无关文件条目的 tar 构建器。
        let mut builder = tar::Builder::new(encoder);
        // Tar header describing the unrelated archive entry.
        // 描述无关归档条目的 tar 头。
        let mut header = tar::Header::new_gnu();
        // Unrelated archive body that proves the archive can be read successfully.
        // 证明归档可被成功读取的无关条目内容。
        let body = b"unrelated payload";
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "other.txt", &body[..])
            .expect("unrelated tar entry should be written");
        // Finished gzip encoder returned after the tar stream is finalized.
        // tar 流收尾后返回的 gzip 编码器。
        let encoder = builder.into_inner().expect("tar builder should finish");
        encoder.finish().expect("gzip encoder should finish");
        // InstallRoot already contains a stale regular file at the required export destination.
        // InstallRoot 已在必需导出目标位置包含一个陈旧普通文件。
        let install_root = temp_root.join("install");
        // ExportTarget reproduces a partial or old installation that must not prove an archive match.
        // ExportTarget 复现不能用于证明归档命中的部分安装或旧安装。
        let export_target = install_root.join("bin").join("demo");
        std::fs::create_dir_all(
            export_target
                .parent()
                .expect("export target should have a parent"),
        )
        .expect("export target parent should be created");
        std::fs::write(&export_target, b"stale payload")
            .expect("stale export target should be written");
        // Export declaration whose archive path is intentionally absent from the tar.gz payload.
        // 归档路径有意不存在于 tar.gz 载荷中的导出声明。
        let exports = vec![DependencyExportSpec {
            archive_path: "bin/demo".to_string(),
            target_path: "bin/demo".to_string(),
            executable: false,
        }];

        // Error returned because this archive did not produce the declaration during this attempt.
        // 因本次归档没有在当前尝试中产出该声明而返回的错误。
        let error = install_downloaded_payload(
            &archive_path,
            DependencyArchiveType::TarGz,
            &install_root,
            &exports,
        )
        .expect_err("stale file must not satisfy a missing archive export");

        assert!(
            error.contains("does not contain required export 'bin/demo'"),
            "unexpected error: {}",
            error
        );
        assert_eq!(
            std::fs::read(&export_target).expect("stale target should remain readable"),
            b"stale payload",
            "unmatched stale target should remain untouched"
        );
        // Cleanup result is intentionally ignored for best-effort temporary test artifacts.
        // 对临时测试产物的清理结果按最佳努力原则有意忽略。
        let _ = std::fs::remove_dir_all(&temp_root);
    }

    /// Verify one tar.gz source entry can be streamed once and exported to multiple destinations.
    /// 验证单个 tar.gz 源条目可流式读取一次并导出到多个目标。
    #[test]
    fn tar_gz_install_exports_one_source_to_multiple_targets() {
        // TempRoot isolates the generated archive and both declared destinations.
        // TempRoot 隔离生成的归档与两个声明目标。
        let temp_root = std::env::temp_dir().join(format!(
            "luaskills_archive_multi_target_export_test_{}",
            std::process::id()
        ));
        if temp_root.exists() {
            // Stale fixture cleanup result is intentionally ignored before recreation.
            // 重建前对陈旧夹具的清理结果有意忽略。
            let _ = std::fs::remove_dir_all(&temp_root);
        }
        std::fs::create_dir_all(&temp_root).expect("temp root should be created");
        // ArchivePath contains exactly one source entry consumed by two export declarations.
        // ArchivePath 只包含一个被两个导出声明消费的源条目。
        let archive_path = temp_root.join("payload.tar.gz");
        // ArchiveFile receives the gzip-compressed tar stream.
        // ArchiveFile 接收 gzip 压缩的 tar 流。
        let archive_file =
            std::fs::File::create(&archive_path).expect("tar.gz archive should be created");
        // Encoder compresses the tar stream incrementally.
        // Encoder 增量压缩 tar 流。
        let encoder = flate2::write::GzEncoder::new(archive_file, flate2::Compression::default());
        // Builder emits the single shared source entry.
        // Builder 写入唯一的共享源条目。
        let mut builder = tar::Builder::new(encoder);
        // Header describes the shared source payload.
        // Header 描述共享源载荷。
        let mut header = tar::Header::new_gnu();
        // Body is compared byte-for-byte at both destinations.
        // Body 会在两个目标位置逐字节比较。
        let body = b"shared payload";
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "package/bin/shared.dat", &body[..])
            .expect("shared tar entry should be written");
        // FinishedEncoder is returned after finalizing the tar stream.
        // FinishedEncoder 在 tar 流完成后返回。
        let finished_encoder = builder.into_inner().expect("tar builder should finish");
        finished_encoder
            .finish()
            .expect("gzip encoder should finish");
        // InstallRoot owns both output paths.
        // InstallRoot 拥有两个输出路径。
        let install_root = temp_root.join("install");
        // Exports deliberately reuse one archive path with distinct targets.
        // Exports 有意让两个不同目标复用同一归档路径。
        let exports = vec![
            DependencyExportSpec {
                archive_path: "bin/shared.dat".to_string(),
                target_path: "first/shared.dat".to_string(),
                executable: false,
            },
            DependencyExportSpec {
                archive_path: "bin/shared.dat".to_string(),
                target_path: "second/shared.dat".to_string(),
                executable: false,
            },
        ];

        install_downloaded_payload(
            &archive_path,
            DependencyArchiveType::TarGz,
            &install_root,
            &exports,
        )
        .expect("one tar source should satisfy both declarations");

        assert_eq!(
            std::fs::read(install_root.join("first").join("shared.dat"))
                .expect("first export should be readable"),
            body
        );
        assert_eq!(
            std::fs::read(install_root.join("second").join("shared.dat"))
                .expect("second export should be readable"),
            body
        );
        // Cleanup result is intentionally ignored for best-effort temporary test artifacts.
        // 对临时测试产物的清理结果按最佳努力原则有意忽略。
        let _ = std::fs::remove_dir_all(&temp_root);
    }
}
