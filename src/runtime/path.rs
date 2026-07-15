use std::path::{Path, PathBuf};

/// Return whether one Windows path suffix is an absolute drive path with one separator style.
/// 返回一个 Windows 路径后缀是否为采用单一分隔符风格的绝对盘符路径。
///
/// `text` is the portion after a possible verbatim prefix and `separator` is the only path
/// delimiter allowed by that textual form.
/// `text` 是可能存在的 verbatim 前缀之后的部分，`separator` 是该文本形式唯一允许的路径分隔符。
#[cfg(windows)]
fn is_absolute_windows_drive_path_text(text: &str, separator: u8) -> bool {
    // UTF-8 bytes inspected only at the ASCII drive/root prefix.
    // 仅在 ASCII 盘符/根前缀处检查的 UTF-8 字节。
    let bytes = text.as_bytes();
    // Opposite separator whose presence would change verbatim path interpretation.
    // 一旦出现便会改变 verbatim 路径解释的另一类分隔符。
    let mixed_separator = match separator {
        b'\\' => text.contains('/'),
        b'/' => text.contains('\\'),
        _ => true,
    };
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && bytes[2] == separator
        && !mixed_separator
}

/// Return whether one verbatim UNC suffix contains a valid server/share pair and one separator style.
/// 返回一个 verbatim UNC 后缀是否包含有效服务器/共享名对并只使用一种分隔符风格。
///
/// `text` is the suffix after `UNC` and its delimiter; `separator` is that delimiter. A single
/// trailing separator is accepted, while mixed, repeated, missing, or empty components are rejected.
/// `text` 是 `UNC` 及其分隔符之后的后缀，`separator` 是该分隔符；允许单个末尾分隔符，
/// 混合、重复、缺失或空组件均会被拒绝。
#[cfg(windows)]
fn is_windows_unc_path_suffix(text: &str, separator: char) -> bool {
    // Opposite separator forbidden inside one canonical textual form.
    // 单一规范文本形式中禁止出现的另一类分隔符。
    let mixed_separator = match separator {
        '\\' => text.contains('/'),
        '/' => text.contains('\\'),
        _ => true,
    };
    if mixed_separator {
        return false;
    }
    // Optional final delimiter removed once so a share root remains valid without accepting doubles.
    // 仅移除一次可选末尾分隔符，使共享根保持合法且不接受双分隔符。
    let body = text.strip_suffix(separator).unwrap_or(text);
    // UNC components retained for server/share cardinality and repetition checks.
    // 用于服务器/共享数量及重复分隔符校验的 UNC 组件。
    let components = body.split(separator).collect::<Vec<_>>();
    components.len() >= 2 && components.iter().all(|component| !component.is_empty())
}

/// Normalize one host-visible path string when its Windows verbatim form has an ordinary equivalent.
/// 当 Windows verbatim 形式具备普通等价路径时，归一化一个宿主可见路径文本。
///
/// The `rendered` parameter is the already-rendered path string before host-visible cleanup.
/// `rendered` 参数是执行宿主可见清理之前已经渲染好的路径字符串。
///
/// Return an equivalent drive/UNC spelling; unsupported namespaces remain unchanged so callers can
/// reject them instead of silently changing path identity.
/// 返回等价盘符/UNC 写法；不受支持的命名空间保持原样，使调用方能够拒绝而非静默改变路径身份。
pub(crate) fn normalize_host_visible_path_text(rendered: &str) -> String {
    #[cfg(windows)]
    {
        // Case-insensitive extended UNC prefix emitted by Windows canonicalization or host APIs.
        // Windows 规范化或宿主 API 产生的大小写不敏感扩展 UNC 前缀。
        if rendered
            .get(..8)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(r"\\?\UNC\"))
        {
            // Standard UNC spelling accepted by Lua, Node.js, and ordinary Win32 path consumers.
            // Lua、Node.js 与普通 Win32 路径消费者均可接受的标准 UNC 写法。
            let stripped = &rendered[8..];
            if is_windows_unc_path_suffix(stripped, '\\') {
                return format!(r"\\{}", stripped);
            }
        }
        // Forward-slash extended UNC spelling accepted from JSON and cross-platform hosts.
        // 从 JSON 与跨平台宿主接受的正斜杠扩展 UNC 写法。
        if rendered
            .get(..8)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("//?/UNC/"))
        {
            // UNC server/share suffix converted to ordinary Windows separators for Lua consumers.
            // 为 Lua 消费方转换为普通 Windows 分隔符的 UNC 服务器与共享后缀。
            let stripped = &rendered[8..];
            if is_windows_unc_path_suffix(stripped, '/') {
                return format!(r"\\{}", stripped.replace('/', r"\"));
            }
        }
        if let Some(stripped) = rendered.strip_prefix(r"\\?\")
            && is_absolute_windows_drive_path_text(stripped, b'\\')
        {
            return stripped.to_string();
        }
        if let Some(stripped) = rendered.strip_prefix("//?/")
            && is_absolute_windows_drive_path_text(stripped, b'/')
        {
            return stripped.to_string();
        }
    }
    rendered.to_string()
}

/// Normalize one host/API-supplied path and reject verbatim namespaces without ordinary equivalents.
/// 归一化一个宿主/API 提供的路径，并拒绝不存在普通等价形式的 verbatim 命名空间。
///
/// The `rendered` parameter is an untrusted UTF-8 path received from Lua, JSON, or an FFI request.
/// `rendered` 参数是从 Lua、JSON 或 FFI 请求收到的不可信 UTF-8 路径。
///
/// Return a Lua-compatible drive/UNC spelling, or a stable error for unsupported Windows device,
/// volume, pipe, mixed-separator, and other verbatim namespaces.
/// 返回 Lua 兼容的盘符/UNC 写法；Windows 设备、卷、管道、混合分隔符及其他不受支持的
/// verbatim 命名空间会返回稳定错误。
pub(crate) fn normalize_host_input_path_text(rendered: &str) -> Result<String, String> {
    let normalized = normalize_host_visible_path_text(rendered);
    #[cfg(windows)]
    if normalized.starts_with(r"\\?\") || normalized.starts_with("//?/") {
        return Err("unsupported Windows verbatim path namespace".to_string());
    }
    Ok(normalized)
}

/// Render one filesystem path for host-visible runtime surfaces using an ordinary drive/UNC spelling.
/// 使用普通盘符/UNC 写法为宿主可见运行时表面渲染文件系统路径。
///
/// The `path` parameter is the filesystem path that should be exposed to hosts or Lua runtime surfaces.
/// `path` 参数是需要暴露给宿主或 Lua 运行时表面的文件系统路径。
///
/// Return the host-visible path after lossy OS-string rendering and safe prefix cleanup; callers
/// accepting untrusted paths must use `normalize_host_input_path_text` to reject other namespaces.
/// 返回经过 OS 字符串有损渲染与安全前缀清理后的宿主可见路径；接受不可信路径的调用方必须使用
/// `normalize_host_input_path_text` 拒绝其他命名空间。
pub fn render_host_visible_path(path: &Path) -> String {
    normalize_host_visible_path_text(&path.to_string_lossy())
}

/// Convert one native Windows verbatim `Path` without lossy UTF-16 transcoding.
/// 在不进行有损 UTF-16 转码的情况下转换一个原生 Windows verbatim `Path`。
///
/// `path` is an already authorized native path. Canonical drive and UNC spellings are converted only
/// when their separator and root structure have an ordinary equivalent.
/// `path` 是已经授权的原生路径；仅当规范盘符与 UNC 写法的分隔符及根结构具备普通等价形式时转换。
///
/// Return the equivalent ordinary path, the original non-verbatim path, or a stable unsupported
/// namespace error without performing a filesystem lookup.
/// 返回等价普通路径、原始非 verbatim 路径，或稳定的不支持命名空间错误，且不执行文件系统查询。
#[cfg(windows)]
pub(crate) fn normalize_windows_verbatim_path(path: &Path) -> Result<PathBuf, String> {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    // Exact UTF-16 path units retained throughout namespace inspection and reconstruction.
    // 在命名空间检查与重建全程保留的精确 UTF-16 路径单元。
    let encoded = path.as_os_str().encode_wide().collect::<Vec<_>>();
    // Canonical local verbatim introducer shared by drive and UNC spellings.
    // 盘符与 UNC 写法共享的规范本地 verbatim 引导符。
    const VERBATIM_PREFIX: [u16; 4] = [92, 92, 63, 92];
    if !encoded.starts_with(&VERBATIM_PREFIX) {
        return Ok(path.to_path_buf());
    }

    // Case-insensitive canonical UNC header followed by one backslash delimiter.
    // 以单个反斜杠分隔符结尾的大小写不敏感规范 UNC 头。
    let is_unc = encoded.len() >= 8
        && matches!(encoded[4], 85 | 117)
        && matches!(encoded[5], 78 | 110)
        && matches!(encoded[6], 67 | 99)
        && encoded[7] == 92;
    if is_unc {
        // UNC server/share suffix whose component structure must survive prefix conversion exactly.
        // 前缀转换后组件结构必须精确保留的 UNC 服务器/共享后缀。
        let suffix = &encoded[8..];
        // Backslash-delimited components with one optional trailing delimiter removed.
        // 以反斜杠分隔并移除一个可选末尾分隔符的组件。
        let mut components = suffix.split(|unit| *unit == 92).collect::<Vec<_>>();
        if components
            .last()
            .is_some_and(|component| component.is_empty())
        {
            components.pop();
        }
        // Valid UNC pair excluding mixed separators, empty names, and repeated delimiters.
        // 排除混合分隔符、空名称与重复分隔符的有效 UNC 对。
        let valid_unc = !suffix.contains(&47)
            && components.len() >= 2
            && components.iter().all(|component| !component.is_empty());
        if valid_unc {
            // Ordinary leading UNC slashes followed by the exact original suffix units.
            // 普通 UNC 前导双反斜杠及精确保留的原始后缀单元。
            let mut normalized = Vec::with_capacity(encoded.len().saturating_sub(6));
            normalized.extend_from_slice(&[92, 92]);
            normalized.extend_from_slice(suffix);
            return Ok(PathBuf::from(OsString::from_wide(&normalized)));
        }
        return Err("unsupported Windows verbatim path namespace".to_string());
    }

    // Local suffix accepted only for an absolute drive path using backslashes exclusively.
    // 仅当本地后缀是完全使用反斜杠的绝对盘符路径时接受。
    let remainder = &encoded[VERBATIM_PREFIX.len()..];
    let drive_qualified = remainder.len() >= 3
        && (remainder[0] >= u16::from(b'A') && remainder[0] <= u16::from(b'Z')
            || remainder[0] >= u16::from(b'a') && remainder[0] <= u16::from(b'z'))
        && remainder[1] == u16::from(b':')
        && remainder[2] == u16::from(b'\\')
        && !remainder.contains(&u16::from(b'/'));
    if drive_qualified {
        return Ok(PathBuf::from(OsString::from_wide(remainder)));
    }
    Err("unsupported Windows verbatim path namespace".to_string())
}

/// Return a child-process path argument with a safe ordinary Windows equivalent when available.
/// 当存在安全普通 Windows 等价路径时，返回对应的子进程路径参数。
///
/// `path` is an already validated canonical path passed through argv or environment variables.
/// `path` 是已经完成校验、将通过 argv 或环境变量传递的规范路径。
///
/// Node.js 24 treats a verbatim drive path as the drive root, so valid Unicode drive/UNC paths use
/// the equivalent host-visible spelling. Unsupported or non-Unicode Windows paths and all Unix
/// paths retain their original native identity.
/// Node.js 24 会把 verbatim 盘符路径当作盘符根目录，因此合法 Unicode 盘符/UNC 路径使用等价
/// 宿主可见写法；不受支持或非 Unicode 的 Windows 路径以及全部 Unix 路径保留原始原生身份。
pub(crate) fn host_process_path_argument(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        // Validated canonical paths use non-lossy conversion; unexpected namespaces retain identity.
        // 已校验规范路径使用非有损转换；意外命名空间保留原始身份。
        normalize_windows_verbatim_path(path).unwrap_or_else(|_| path.to_path_buf())
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::{host_process_path_argument, normalize_host_input_path_text};
    #[cfg(windows)]
    use super::{normalize_host_visible_path_text, normalize_windows_verbatim_path};
    use std::path::Path;

    /// Verify all supported Windows verbatim spellings become ordinary host-visible paths.
    /// 验证全部受支持的 Windows 逐字路径写法都会转为普通宿主可见路径。
    #[cfg(windows)]
    #[test]
    fn normalize_host_visible_path_text_handles_case_and_separator_variants() {
        assert_eq!(
            normalize_host_visible_path_text(r"\\?\unc\server\share\module.lua"),
            r"\\server\share\module.lua"
        );
        assert_eq!(
            normalize_host_visible_path_text("//?/UNC/server/share/module.lua"),
            r"\\server\share\module.lua"
        );
        assert_eq!(
            normalize_host_visible_path_text("//?/C:/runtime/module.lua"),
            "C:/runtime/module.lua"
        );
        // Unsupported namespaces remain intact so they can never be reinterpreted as relative paths.
        // 不受支持的命名空间保持原样，绝不会被重新解释为相对路径。
        assert_eq!(
            normalize_host_visible_path_text(
                r"\\?\Volume{00000000-0000-0000-0000-000000000000}\module.lua"
            ),
            r"\\?\Volume{00000000-0000-0000-0000-000000000000}\module.lua"
        );
        assert_eq!(
            normalize_host_visible_path_text(r"\\?\C:/runtime/module.lua"),
            r"\\?\C:/runtime/module.lua"
        );
        assert_eq!(
            normalize_host_visible_path_text(r"\\?\UNC\server/share\module.lua"),
            r"\\?\UNC\server/share\module.lua"
        );
    }

    /// Verify host/API path normalization rejects verbatim namespaces without safe equivalents.
    /// 验证宿主/API 路径归一化会拒绝不存在安全等价形式的 verbatim 命名空间。
    #[cfg(windows)]
    #[test]
    fn normalize_host_input_path_text_rejects_unsupported_verbatim_namespaces() {
        // Volume-namespace error proving unsupported identities are never rewritten.
        // 卷命名空间错误用于证明不受支持的身份绝不会被改写。
        let error = normalize_host_input_path_text(
            r"\\?\Volume{00000000-0000-0000-0000-000000000000}\module.lua",
        )
        .expect_err("volume GUID namespace must be rejected");
        assert_eq!(error, "unsupported Windows verbatim path namespace");

        // Prefix-level separator mismatch rejected before UNC parsing.
        // 在 UNC 解析前拒绝的前缀级分隔符不匹配。
        let mixed_error = normalize_host_input_path_text(r"\\?\UNC/server/share/module.lua")
            .expect_err("mixed-separator UNC namespace must be rejected");
        assert_eq!(mixed_error, "unsupported Windows verbatim path namespace");

        // Suffix-level separator mismatch rejected after recognizing the UNC prefix.
        // 识别 UNC 前缀后拒绝的后缀级分隔符不匹配。
        let mixed_suffix_error = normalize_host_input_path_text(r"\\?\UNC\server/share\module.lua")
            .expect_err("mixed-separator UNC suffix must be rejected");
        assert_eq!(
            mixed_suffix_error,
            "unsupported Windows verbatim path namespace"
        );

        // Drive-path separator mismatch rejected instead of changing verbatim semantics.
        // 拒绝盘符路径分隔符不匹配，避免改变 verbatim 语义。
        let mixed_drive_error = normalize_host_input_path_text(r"\\?\C:/runtime/module.lua")
            .expect_err("mixed-separator drive path must be rejected");
        assert_eq!(
            mixed_drive_error,
            "unsupported Windows verbatim path namespace"
        );
    }

    /// Verify native UTF-16 path conversion shares the strict drive/UNC namespace contract.
    /// 验证原生 UTF-16 路径转换会共享严格的盘符/UNC 命名空间契约。
    #[cfg(windows)]
    #[test]
    fn normalize_windows_verbatim_path_is_non_lossy_and_strict() {
        // Ordinary UNC path reconstructed from a case-insensitive canonical prefix.
        // 从大小写不敏感规范前缀重建的普通 UNC 路径。
        let unc = normalize_windows_verbatim_path(Path::new(r"\\?\unc\server\share\module.lua"))
            .expect("normalize native UNC path");
        assert_eq!(unc, Path::new(r"\\server\share\module.lua"));

        // Mixed native drive spelling rejected rather than reinterpreted by child runtimes.
        // 拒绝混合原生盘符写法，避免被子运行时重新解释。
        let mixed_error = normalize_windows_verbatim_path(Path::new(r"\\?\C:/runtime\module.lua"))
            .expect_err("mixed native drive path must fail");
        assert_eq!(mixed_error, "unsupported Windows verbatim path namespace");
    }

    /// Verify Node entry paths use the platform-native spelling accepted by the runtime.
    /// 验证 Node 入口路径使用运行时可接受的平台原生写法。
    #[test]
    fn host_process_path_argument_uses_runtime_compatible_path() {
        #[cfg(windows)]
        assert_eq!(
            host_process_path_argument(Path::new(r"\\?\C:\runtime\pnpm.cjs")),
            Path::new(r"C:\runtime\pnpm.cjs")
        );
        #[cfg(not(windows))]
        assert_eq!(
            host_process_path_argument(Path::new("/runtime/pnpm.cjs")),
            Path::new("/runtime/pnpm.cjs")
        );
        #[cfg(windows)]
        assert_eq!(
            host_process_path_argument(Path::new(
                r"\\?\Volume{00000000-0000-0000-0000-000000000000}\pnpm.cjs"
            )),
            Path::new(r"\\?\Volume{00000000-0000-0000-0000-000000000000}\pnpm.cjs")
        );
    }
}
