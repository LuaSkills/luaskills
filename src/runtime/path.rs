use std::path::Path;

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
