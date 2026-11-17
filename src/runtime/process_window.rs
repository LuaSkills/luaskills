use std::os::windows::process::CommandExt;
use std::process::Command;
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

use crate::LuaRuntimeHostMode;

/// Apply the exact platform creation flags and the immutable engine window policy.
/// 应用精确的平台创建标志与不可变引擎窗口策略。
/// `command` is the unspawned child; `host_mode` selects visibility; `base_flags` preserves lifecycle flags.
/// `command` 是尚未启动的子进程；`host_mode` 选择可见性；`base_flags` 保留生命周期标志。
/// Returns unit after configuring the Windows child process.
/// 配置 Windows 子进程后返回空值。
pub(crate) fn configure_command(
    command: &mut Command,
    host_mode: LuaRuntimeHostMode,
    base_flags: u32,
) {
    // Recompute the complete mask on every attempt so Job retries cannot erase the window policy.
    // 每次尝试都重新计算完整掩码，防止 Job 重试清除窗口策略。
    let flags = match host_mode {
        LuaRuntimeHostMode::Console => base_flags,
        LuaRuntimeHostMode::Desktop => base_flags | CREATE_NO_WINDOW,
    };
    command.creation_flags(flags);
}
