/// Every platform key a LuaSkills package may declare, in stable order.
/// LuaSkills 技能包可以声明的全部平台键，按稳定顺序排列。
pub const SUPPORTED_PLATFORM_KEYS: [&str; 6] = [
    "windows-x64",
    "windows-arm64",
    "linux-x64",
    "linux-arm64",
    "macos-x64",
    "macos-arm64",
];

/// Return whether one platform key is a supported LuaSkills platform key.
/// 返回单个平台键是否为受支持的 LuaSkills 平台键。
pub fn is_supported_platform_key(key: &str) -> bool {
    SUPPORTED_PLATFORM_KEYS.contains(&key)
}

/// Build the normalized LuaSkills platform key for the current target runtime.
/// 构建当前目标运行时使用的标准 LuaSkills 平台键。
pub fn current_platform_key() -> &'static str {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "windows-x64"
    } else if cfg!(all(target_os = "windows", target_arch = "aarch64")) {
        "windows-arm64"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "linux-x64"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "linux-arm64"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "macos-x64"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "macos-arm64"
    } else {
        "unknown"
    }
}
