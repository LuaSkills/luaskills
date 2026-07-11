use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use tar::Archive;
use zip::ZipArchive;

use crate::runtime::path::render_host_visible_path;
use crate::skill::dependencies::{DependencyArchiveType, DependencyExportSpec};

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

/// Inspect whether one installed export target path is a file without hiding filesystem probe errors.
/// 检查单个已安装导出目标路径是否为文件，同时不隐藏文件系统探测错误。
///
/// The target_path parameter is the concrete installed file path expected after archive extraction.
/// target_path 参数是归档解压后预期存在的具体已安装文件路径。
///
/// Return true for an existing export file, false for a confirmed missing export file, or an explicit probe/type error.
/// 已存在导出文件返回 true，确认缺失导出文件返回 false；探测或类型异常时返回显式错误。
fn installed_export_target_is_file(target_path: &Path) -> Result<bool, String> {
    match fs::metadata(target_path) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(format!(
            "Installed export target is not a file: {}",
            render_archive_path(target_path)
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "Failed to inspect installed export target {}: {}",
            render_archive_path(target_path),
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
    fs::create_dir_all(install_root).map_err(|error| {
        format!(
            "Failed to create {}: {}",
            render_archive_path(install_root),
            error
        )
    })?;
    match archive_type {
        DependencyArchiveType::Raw => install_from_raw_file(archive_path, install_root, exports),
        DependencyArchiveType::Zip => install_from_zip_archive(archive_path, install_root, exports),
        DependencyArchiveType::TarGz => {
            install_from_tar_gz_archive(archive_path, install_root, exports)
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
    install_root: &Path,
    exports: &[DependencyExportSpec],
) -> Result<(), String> {
    if exports.len() != 1 {
        return Err("raw dependency payload must declare exactly one export".to_string());
    }
    let export = &exports[0];
    let target_path = join_relative_target(install_root, &export.target_path);
    copy_file_with_parent_dir(archive_path, &target_path)?;
    mark_executable_if_needed(&target_path, export.executable)?;
    Ok(())
}

/// Install exports from one zip archive payload.
/// 从单个 zip 归档载荷中安装导出文件。
fn install_from_zip_archive(
    archive_path: &Path,
    install_root: &Path,
    exports: &[DependencyExportSpec],
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
        let entry_name = resolve_zip_export_entry_name(&mut archive, &export.archive_path)
            .ok_or_else(|| {
                format!(
                    "Failed to read zip entry '{}' from {}: specified file not found in archive",
                    export.archive_path,
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
        let target_path = join_relative_target(install_root, &export.target_path);
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
                export.archive_path,
                render_archive_path(&target_path),
                error
            )
        })?;
        mark_executable_if_needed(&target_path, export.executable)?;
    }
    Ok(())
}

/// Install exports from one tar.gz archive payload.
/// 从单个 tar.gz 归档载荷中安装导出文件。
fn install_from_tar_gz_archive(
    archive_path: &Path,
    install_root: &Path,
    exports: &[DependencyExportSpec],
) -> Result<(), String> {
    let bytes = fs::read(archive_path).map_err(|error| {
        format!(
            "Failed to read {}: {}",
            render_archive_path(archive_path),
            error
        )
    })?;
    let decoder = GzDecoder::new(Cursor::new(bytes));
    let mut archive = Archive::new(decoder);
    let mut extracted_entries: Vec<(PathBuf, bool)> = Vec::new();

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
        if let Some(export) = exports
            .iter()
            .find(|export| archive_entry_matches_export(&entry_path, &export.archive_path))
        {
            let target_path = join_relative_target(install_root, &export.target_path);
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
            let mut buffer = Vec::new();
            archive_entry.read_to_end(&mut buffer).map_err(|error| {
                format!(
                    "Failed to extract '{}' from {}: {}",
                    export.archive_path,
                    render_archive_path(archive_path),
                    error
                )
            })?;
            std::io::copy(&mut Cursor::new(buffer), &mut output).map_err(|error| {
                format!(
                    "Failed to write {}: {}",
                    render_archive_path(&target_path),
                    error
                )
            })?;
            extracted_entries.push((target_path, export.executable));
        }
    }

    for export in exports {
        let target_path = join_relative_target(install_root, &export.target_path);
        if !installed_export_target_is_file(&target_path)? {
            return Err(format!(
                "tar.gz archive {} does not contain required export '{}'",
                render_archive_path(archive_path),
                export.archive_path
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
    if archive
        .file_names()
        .any(|name| normalize_archive_entry_match_path(name) == expected)
    {
        return Some(expected);
    }

    archive.file_names().find_map(|name| {
        let normalized_name = normalize_archive_entry_match_path(name);
        if strip_one_leading_archive_component(&normalized_name).as_deref()
            == Some(expected.as_str())
        {
            Some(normalized_name)
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

/// Join one relative target path under the dependency root.
/// 把单个相对目标路径拼接到依赖根目录下。
fn join_relative_target(root: &Path, relative_target: &str) -> PathBuf {
    let normalized = relative_target.replace('/', std::path::MAIN_SEPARATOR_STR);
    root.join(normalized)
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

    /// Verify tar.gz install rejects directory export targets when a required export is missing.
    /// 验证 tar.gz 安装在必需导出缺失时会拒绝目录型导出目标。
    #[test]
    fn tar_gz_install_rejects_directory_export_target_when_export_missing() {
        // Temporary root that isolates the tar.gz export target fixture.
        // 隔离 tar.gz 导出目标夹具的临时根目录。
        let temp_root = std::env::temp_dir().join(format!(
            "luaskills_archive_directory_export_target_test_{}",
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
        // Install root that already contains a directory where the required export file should appear.
        // 已在必需导出文件目标位置包含目录的安装根目录。
        let install_root = temp_root.join("install");
        // Directory occupying the declared export target path.
        // 占据声明导出目标路径的目录。
        let export_target = install_root.join("bin").join("demo");
        std::fs::create_dir_all(&export_target).expect("directory export target should be created");
        // Export declaration whose archive path is intentionally absent from the tar.gz payload.
        // 归档路径有意不存在于 tar.gz 载荷中的导出声明。
        let exports = vec![DependencyExportSpec {
            archive_path: "bin/demo".to_string(),
            target_path: "bin/demo".to_string(),
            executable: false,
        }];

        // Error returned by the real install entrypoint before a directory can satisfy a file export.
        // 在目录满足文件导出之前由真实安装入口返回的错误。
        let error = install_downloaded_payload(
            &archive_path,
            DependencyArchiveType::TarGz,
            &install_root,
            &exports,
        )
        .expect_err("directory export target should fail missing export validation");

        assert!(
            error.contains("Installed export target is not a file"),
            "unexpected error: {}",
            error
        );
        assert!(
            error.contains(&render_host_visible_path(&export_target)),
            "unexpected error: {}",
            error
        );
        assert!(
            export_target.is_dir(),
            "directory export target should remain after rejection"
        );
        // Cleanup result is intentionally ignored for best-effort temporary test artifacts.
        // 对临时测试产物的清理结果按最佳努力原则有意忽略。
        let _ = std::fs::remove_dir_all(&temp_root);
    }

    /// Verify installed export target probes report invalid path errors.
    /// 验证已安装导出目标探测会报告非法路径错误。
    #[test]
    fn installed_export_target_probe_errors_are_reported() {
        // Installed export target path containing one embedded NUL that metadata cannot inspect.
        // 包含内嵌 NUL 且元数据无法探测的已安装导出目标路径。
        let invalid_target_path = PathBuf::from("invalid\0target");

        // Error returned before the invalid target can behave like a missing export target.
        // 在非法目标表现得像缺失导出目标之前返回的错误。
        let error = installed_export_target_is_file(&invalid_target_path)
            .expect_err("invalid installed export target probe should fail");

        assert!(
            error.contains("Failed to inspect installed export target"),
            "unexpected error: {}",
            error
        );
        assert!(error.contains("invalid"), "unexpected error: {}", error);
    }
}
