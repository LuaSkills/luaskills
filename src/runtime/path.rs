use std::path::{Path, PathBuf};

/// Normalize one host-visible path string so Windows verbatim prefixes never leak into public runtime surfaces.
/// 归一化一个宿主可见路径文本，避免 Windows verbatim 前缀泄漏到公开运行时表面。
///
/// The `rendered` parameter is the already-rendered path string before host-visible cleanup.
/// `rendered` 参数是执行宿主可见清理之前已经渲染好的路径字符串。
///
/// Return the path string safe to expose through host-facing runtime structures.
/// 返回可以通过宿主侧运行时结构暴露的路径字符串。
pub(crate) fn normalize_host_visible_path_text(rendered: &str) -> String {
    #[cfg(windows)]
    {
        if let Some(stripped) = rendered.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{}", stripped);
        }
        if let Some(stripped) = rendered.strip_prefix(r"\\?\") {
            return stripped.to_string();
        }
    }
    rendered.to_string()
}

/// Render one filesystem path for host-visible runtime surfaces without Windows verbatim prefixes.
/// 为宿主可见运行时表面渲染文件系统路径，并去掉 Windows verbatim 前缀。
///
/// The `path` parameter is the filesystem path that should be exposed to hosts or Lua runtime surfaces.
/// `path` 参数是需要暴露给宿主或 Lua 运行时表面的文件系统路径。
///
/// Return the host-visible path string after lossy OS-string rendering and prefix cleanup.
/// 返回经过 OS 字符串有损渲染与前缀清理后的宿主可见路径字符串。
pub fn render_host_visible_path(path: &Path) -> String {
    normalize_host_visible_path_text(&path.to_string_lossy())
}

/// Return a child-process path argument without a Windows verbatim prefix.
/// 返回不含 Windows verbatim 前缀的子进程路径参数。
///
/// `path` is an already validated canonical path passed through argv or environment variables.
/// `path` 是已经完成校验、将通过 argv 或环境变量传递的规范路径。
///
/// Node.js 24 treats a verbatim drive path as the drive root, so Windows receives the equivalent
/// host-visible spelling while Unix retains the original native path bytes.
/// Node.js 24 会把 verbatim 盘符路径当作盘符根目录，因此 Windows 使用等价的宿主可见写法，
/// Unix 则保留原始原生路径字节。
pub(crate) fn host_process_path_argument(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(normalize_host_visible_path_text(&path.to_string_lossy()))
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::host_process_path_argument;
    use std::path::Path;

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
    }
}
