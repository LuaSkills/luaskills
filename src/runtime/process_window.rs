use std::process::Command;

use crate::LuaRuntimeHostMode;

/// Apply the exact platform creation flags and the immutable engine window policy.
/// 应用精确的平台创建标志与不可变引擎窗口策略。
/// `command` is the unspawned child; `host_mode` selects visibility; `base_flags` preserves lifecycle flags.
/// `command` 是尚未启动的子进程；`host_mode` 选择可见性；`base_flags` 保留生命周期标志。
/// Returns unit after configuring Windows; on other platforms this leaves the command unchanged.
/// 配置 Windows 后返回空值；其他平台保持命令不变。
pub(crate) fn configure_command(
    command: &mut Command,
    host_mode: LuaRuntimeHostMode,
    base_flags: u32,
) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

        // Recompute the complete mask on every attempt so Job retries cannot erase the window policy.
        // 每次尝试都重新计算完整掩码，防止 Job 重试清除窗口策略。
        let flags = match host_mode {
            LuaRuntimeHostMode::Console => base_flags,
            LuaRuntimeHostMode::Desktop => base_flags | CREATE_NO_WINDOW,
        };
        command.creation_flags(flags);
    }
    #[cfg(not(windows))]
    let _ = (command, host_mode, base_flags);
}
