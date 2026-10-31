pub mod cache;
pub(crate) mod config;
pub(crate) mod config_service;
pub mod config_tool;
pub mod context;
/// Independent host-owned plugin execution contracts and runtime.
/// 独立且由宿主拥有的插件执行契约与运行时。
pub mod embedded;
pub mod encoding;
pub mod engine;
pub mod entry;
pub(crate) mod file_system;
pub(crate) mod file_watcher;
pub mod help;
pub mod logging;
pub mod managed_io;
pub(crate) mod managed_package;
pub mod managed_runtime;
pub(crate) mod managed_runtime_services;
pub(crate) mod managed_runtime_session;
pub mod managed_session_events;
#[doc(hidden)]
pub use path::render_host_visible_path;
/// Windows desktop replacements for native Lua command execution and streaming pipes.
/// Windows 桌面模式下原生 Lua 命令执行与流式管道的替代实现。
#[cfg(windows)]
pub(crate) mod native_process;
pub(crate) mod path;
pub mod process_session;
/// Shared platform window policy for every runtime-owned child process.
/// 所有运行库自有子进程共享的平台窗口策略。
pub(crate) mod process_window;
pub mod result;
#[cfg(test)]
pub(crate) mod test_support;
