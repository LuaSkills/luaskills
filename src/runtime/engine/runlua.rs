use super::lease::default_runlua_exec_args;
use super::*;

/// RunLua execution request accepted by `vulcan.runtime.lua.exec`.
/// `vulcan.runtime.lua.exec` 接收的 RunLua 执行请求结构。
#[derive(Debug, Deserialize, Serialize)]
struct RunLuaExecRequest {
    /// Human-readable task summary echoed in the result header.
    /// 展示在结果头部的人类可读任务摘要。
    #[serde(default)]
    task: String,
    /// Inline Lua source code executed inside the isolated runtime VM.
    /// 在隔离运行时虚拟机中执行的内联 Lua 源代码。
    #[serde(default)]
    code: Option<String>,
    /// Lua file path executed inside the isolated runtime VM.
    /// 在隔离运行时虚拟机中执行的 Lua 文件路径。
    #[serde(default)]
    file: Option<String>,
    /// Structured arguments exposed to Lua as `args`.
    /// 以 `args` 变量形式暴露给 Lua 的结构化参数。
    #[serde(default = "default_runlua_exec_args")]
    args: Value,
    /// Maximum execution time in milliseconds. Defaults to 60 seconds.
    /// 最大执行时长（毫秒），默认 60 秒。
    #[serde(default = "default_runlua_timeout_ms")]
    timeout_ms: u64,
    /// Internal caller tool name used to enforce luaexec reentrancy guards.
    /// 用于执行 luaexec 重入保护的内部调用者工具名称。
    #[serde(default)]
    caller_tool_name: Option<String>,
}

/// Runtime dependency snapshot required by isolated runlua execution.
/// 隔离 runlua 执行所需的运行时依赖快照。
#[derive(Clone)]
pub(super) struct RunLuaRuntimeContext {
    /// Dedicated Lua VM pool used only by luaexec and nested runlua calls.
    /// 仅供 luaexec 与嵌套 runlua 调用使用的专用 Lua 虚拟机池。
    runlua_pool: Arc<LuaVmPool>,
    /// Loaded skill table visible to the isolated runlua VM.
    /// 隔离 runlua 虚拟机可见的已加载技能表。
    skills: Arc<HashMap<String, LoadedSkill>>,
    /// Entry registry snapshot visible to nested tool calls from runlua.
    /// runlua 内部嵌套工具调用可见的入口注册表快照。
    entry_registry: Arc<BTreeMap<String, ResolvedEntryTarget>>,
    /// Host options shared with the isolated runlua VM.
    /// 与隔离 runlua 虚拟机共享的宿主选项。
    host_options: Arc<LuaRuntimeHostOptions>,
    /// Unified skill configuration store exposed inside the runlua VM.
    /// 暴露到 runlua 虚拟机内部的统一技能配置存储。
    skill_config_store: Arc<SkillConfigStore>,
    /// Runtime skill roots used for dependency and context resolution.
    /// 用于依赖与上下文解析的运行时技能根列表。
    runtime_skill_roots: Vec<RuntimeSkillRoot>,
    /// Optional LanceDB host bridge available to nested runlua calls.
    /// 嵌套 runlua 调用可用的可选 LanceDB 宿主桥接。
    lancedb_host: Option<Arc<LanceDbSkillHost>>,
    /// Optional SQLite host bridge available to nested runlua calls.
    /// 嵌套 runlua 调用可用的可选 SQLite 宿主桥接。
    sqlite_host: Option<Arc<SqliteSkillHost>>,
}

impl RunLuaRuntimeContext {
    /// Capture one runlua dependency snapshot from the current engine and explicit runtime state.
    /// 从当前引擎与显式运行时状态中捕获一份 runlua 依赖快照。
    pub(super) fn from_engine(
        engine: &LuaEngine,
        skills: Arc<HashMap<String, LoadedSkill>>,
        entry_registry: Arc<BTreeMap<String, ResolvedEntryTarget>>,
    ) -> Self {
        Self {
            runlua_pool: engine.runlua_pool.clone(),
            skills,
            entry_registry,
            host_options: engine.host_options.clone(),
            skill_config_store: engine.skill_config_store.clone(),
            runtime_skill_roots: engine.runtime_skill_roots.clone(),
            lancedb_host: engine.lancedb_host.clone(),
            sqlite_host: engine.sqlite_host.clone(),
        }
    }
}

/// Return the default timeout for runlua execution in milliseconds.
/// 返回 runlua 执行的默认超时时间（毫秒）。
pub(super) fn default_runlua_timeout_ms() -> u64 {
    60_000
}

/// Return the process-wide current-directory guard used by lua file execution.
/// 返回 Lua 文件执行期间用于保护进程工作目录切换的全局互斥锁。
pub(super) fn runlua_cwd_guard() -> &'static Mutex<()> {
    static RUNLUA_CWD_GUARD: OnceLock<Mutex<()>> = OnceLock::new();
    RUNLUA_CWD_GUARD.get_or_init(|| Mutex::new(()))
}

/// Acquire the process-wide runlua current-directory guard, recovering after lock poisoning.
/// 获取进程级 runlua 当前目录保护锁；如果锁已 poison，则恢复继续使用。
pub(super) fn lock_runlua_cwd_guard() -> std::sync::MutexGuard<'static, ()> {
    runlua_cwd_guard()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Acquire one runlua print-capture buffer and return its guard, recovering after lock poisoning.
/// 获取并返回单个 runlua print 捕获缓冲区保护对象；如果锁已 poison，则恢复继续使用。
pub(super) fn lock_runlua_print_capture(
    captured_output: &Arc<Mutex<Vec<String>>>,
) -> std::sync::MutexGuard<'_, Vec<String>> {
    captured_output
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Build the restricted simulated request context used by internal luaexec tool calls.
/// 构建内部 luaexec 工具调用使用的受限模拟请求上下文。
fn build_luaexec_call_request_context() -> RuntimeRequestContext {
    RuntimeRequestContext {
        request_id: None,
        client_name: None,
        transport_name: Some("luaexec_call".to_string()),
        session_id: Some("luaexec-call-internal".to_string()),
        client_info: Some(RuntimeClientInfo {
            kind: Some("runtime".to_string()),
            name: Some("luaexec_call".to_string()),
            version: Some("internal-runtime".to_string()),
        }),
        client_capabilities: json!({}),
    }
}

/// One captured renderable runlua return item.
/// 一项已捕获并可渲染的 runlua 返回值。
#[derive(Debug)]
struct RunLuaRenderedValue {
    /// Render format of the current item, such as `text` or `json`.
    /// 当前项的渲染格式，例如 `text` 或 `json`。
    format: &'static str,
    /// Rendered payload already formatted for Markdown code fences.
    /// 已格式化好的载荷文本，可直接写入 Markdown 代码块。
    content: String,
}

/// Detect whether a string looks like Lua's debug-style coercion output.
/// 检测字符串是否像 Lua 对象被 `tostring` 后生成的调试文本。
fn looks_like_lua_debug_value(text: &str) -> bool {
    ["table: 0x", "function: 0x", "thread: 0x", "userdata: 0x"]
        .iter()
        .any(|prefix| text.starts_with(prefix))
}

/// Validate Windows-specific path syntax conservatively before touching the filesystem.
/// 在真正访问文件系统之前，对 Windows 路径语法做保守校验。
#[cfg(windows)]
pub(super) fn has_invalid_windows_path_syntax(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.starts_with(r"\\?\") {
        return false;
    }

    let first_char = trimmed.chars().next();
    for (index, ch) in trimmed.char_indices() {
        if ch.is_control() {
            return true;
        }
        if matches!(ch, '<' | '>' | '"' | '|' | '?' | '*') {
            return true;
        }
        if ch == ':' {
            let is_drive_prefix =
                index == 1 && first_char.map(|c| c.is_ascii_alphabetic()).unwrap_or(false);
            if !is_drive_prefix {
                return true;
            }
        }
    }
    false
}

/// Require an exact UTF-8 Lua string and reject empty/blank values when needed.
/// 要求参数必须是精确的 UTF-8 Lua 字符串，并在需要时拒绝空值或纯空白值。
pub(super) fn require_string_arg(
    value: LuaValue,
    fn_name: &str,
    param_name: &str,
    allow_blank: bool,
) -> mlua::Result<String> {
    let raw = match value {
        LuaValue::String(text) => text
            .to_str()
            .map_err(|_| {
                mlua::Error::runtime(format!(
                    "{fn_name}: {param_name} must be a valid UTF-8 string"
                ))
            })?
            .to_string(),
        other => {
            return Err(mlua::Error::runtime(format!(
                "{fn_name}: {param_name} must be a string, got {}",
                lua_value_type_name(&other)
            )));
        }
    };

    if !allow_blank && raw.trim().is_empty() {
        return Err(mlua::Error::runtime(format!(
            "{fn_name}: {param_name} must not be empty"
        )));
    }
    if raw.contains('\0') {
        return Err(mlua::Error::runtime(format!(
            "{fn_name}: {param_name} must not contain NUL bytes"
        )));
    }
    Ok(raw)
}

/// Validate path-like text before using it in filesystem operations.
/// 在文件系统函数真正使用路径文本前，先进行统一校验。
fn validate_path_text(text: &str, fn_name: &str, param_name: &str) -> mlua::Result<()> {
    if looks_like_lua_debug_value(text) {
        return Err(mlua::Error::runtime(format!(
            "{fn_name}: {param_name} looks like a coerced Lua object string `{text}`"
        )));
    }

    #[cfg(windows)]
    if has_invalid_windows_path_syntax(text) {
        return Err(mlua::Error::runtime(format!(
            "{fn_name}: {param_name} contains invalid Windows path syntax"
        )));
    }

    Ok(())
}

/// Require a validated path string from Lua input.
/// 从 Lua 输入中提取并校验路径字符串参数。
pub(super) fn require_path_arg(
    value: LuaValue,
    fn_name: &str,
    param_name: &str,
) -> mlua::Result<String> {
    let text = require_string_arg(value, fn_name, param_name, false)?;
    validate_path_text(&text, fn_name, param_name)?;
    Ok(text)
}

/// Read an optional non-negative integer argument from Lua.
/// 从 Lua 读取可选的非负整数参数。
pub(super) fn optional_u64_arg(
    value: LuaValue,
    fn_name: &str,
    param_name: &str,
) -> mlua::Result<Option<u64>> {
    match value {
        LuaValue::Nil => Ok(None),
        LuaValue::Integer(v) if v >= 0 => Ok(Some(v as u64)),
        LuaValue::Number(v) if v.is_finite() && v >= 0.0 && v.fract() == 0.0 => Ok(Some(v as u64)),
        other => Err(mlua::Error::runtime(format!(
            "{fn_name}: {param_name} must be a non-negative integer: {}",
            lua_value_type_name(&other)
        ))),
    }
}

/// Require a Lua table argument without silent coercion.
/// 要求参数必须是 Lua table，禁止静默类型转换。
pub(super) fn require_table_arg(
    value: LuaValue,
    fn_name: &str,
    param_name: &str,
) -> mlua::Result<Table> {
    match value {
        LuaValue::Table(table) => Ok(table),
        other => Err(mlua::Error::runtime(format!(
            "{fn_name}: {param_name} must be a table, got {}",
            lua_value_type_name(&other)
        ))),
    }
}

/// Execution mode supported by `vulcan.exec`.
/// `vulcan.exec` 支持的执行模式。
pub(super) enum ExecMode {
    Shell {
        command: String,
        launcher: ExecShellLauncher,
    },
    Program {
        program: String,
        args: Vec<String>,
    },
}

/// Stable shell launcher identifiers supported by `vulcan.process.exec`.
/// `vulcan.process.exec` 支持的稳定 shell 启动器标识。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ExecShellLauncher {
    Cmd,
    Pwsh,
    Powershell,
    Bash,
    Zsh,
    Sh,
}

impl ExecShellLauncher {
    /// Return the stable Lua-visible shell parameter value for one launcher.
    /// 返回单个启动器对 Lua 可见的稳定 shell 参数值。
    fn id(self) -> &'static str {
        match self {
            Self::Cmd => "cmd",
            Self::Pwsh => "pwsh",
            Self::Powershell => "powershell",
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Sh => "sh",
        }
    }

    /// Return the executable program name used to spawn one launcher.
    /// 返回启动单个启动器时使用的可执行程序名。
    fn program(self) -> &'static str {
        match self {
            Self::Cmd => "cmd.exe",
            Self::Pwsh => "pwsh",
            Self::Powershell => "powershell",
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Sh => "sh",
        }
    }

    /// Return the command arguments consumed by one launcher for inline command text.
    /// 返回单个启动器承载内联命令文本时使用的命令参数序列。
    pub(super) fn command_args(self, command_text: &str) -> Vec<String> {
        match self {
            Self::Cmd => vec![String::from("/C"), command_text.to_string()],
            Self::Pwsh | Self::Powershell => vec![
                String::from("-NoProfile"),
                String::from("-Command"),
                command_text.to_string(),
            ],
            Self::Bash | Self::Zsh => vec![String::from("-lc"), command_text.to_string()],
            Self::Sh => vec![String::from("-c"), command_text.to_string()],
        }
    }
}

/// Parsed `shell` field selection used by one process-exec request.
/// 单个 process-exec 请求中解析得到的 `shell` 字段选择结果。
enum ExecShellSetting {
    UseDefault,
    Disabled,
    Selected(ExecShellLauncher),
}

/// Parsed process execution request from Lua.
/// 从 Lua 解析得到的进程执行请求。
pub(super) struct ExecRequest {
    /// Process launch mode requested by Lua.
    /// Lua 请求的进程启动模式。
    mode: ExecMode,
    /// Optional process working directory.
    /// 可选的进程工作目录。
    cwd: Option<String>,
    /// Environment variables applied to the child process.
    /// 应用到子进程的环境变量。
    env: HashMap<String, String>,
    /// Optional text written to child process stdin.
    /// 可选的子进程标准输入文本。
    stdin: Option<String>,
    /// Optional process timeout in milliseconds.
    /// 可选的进程超时时间（毫秒）。
    timeout_ms: Option<u64>,
    /// Encoding used to decode captured stdout bytes.
    /// 用于解码已捕获 stdout 字节的编码。
    stdout_encoding: RuntimeTextEncoding,
    /// Encoding used to decode captured stderr bytes.
    /// 用于解码已捕获 stderr 字节的编码。
    stderr_encoding: RuntimeTextEncoding,
    /// Encoding used to encode stdin text bytes.
    /// 用于编码 stdin 文本字节的编码。
    stdin_encoding: RuntimeTextEncoding,
}

/// Process execution result returned back to Lua.
/// 返回给 Lua 的进程执行结果。
pub(super) struct ExecResult {
    /// Whether the process completed successfully.
    /// 进程是否成功完成。
    ok: bool,
    /// Whether the process completed successfully without timeout.
    /// 进程是否未超时且成功完成。
    success: bool,
    /// Process exit code when available.
    /// 可用时的进程退出码。
    code: Option<i32>,
    /// Decoded stdout text or Base64 text in byte-preserving mode.
    /// 已解码 stdout 文本，或字节保留模式下的 Base64 文本。
    stdout: String,
    /// Decoded stderr text or Base64 text in byte-preserving mode.
    /// 已解码 stderr 文本，或字节保留模式下的 Base64 文本。
    stderr: String,
    /// Whether the process timed out.
    /// 进程是否超时。
    timed_out: bool,
    /// Process-level error summary when execution failed.
    /// 执行失败时的进程级错误摘要。
    error: Option<String>,
    /// Actual stdout encoding used by the decoder.
    /// 解码器实际使用的 stdout 编码。
    stdout_encoding: String,
    /// Actual stderr encoding used by the decoder.
    /// 解码器实际使用的 stderr 编码。
    stderr_encoding: String,
    /// Whether stdout decoding used replacement or fallback behavior.
    /// stdout 解码是否使用了替换或兜底行为。
    stdout_lossy: bool,
    /// Whether stderr decoding used replacement or fallback behavior.
    /// stderr 解码是否使用了替换或兜底行为。
    stderr_lossy: bool,
    /// Byte-preserving stdout payload when available.
    /// 可用时的 stdout 字节保留载荷。
    stdout_base64: Option<String>,
    /// Byte-preserving stderr payload when available.
    /// 可用时的 stderr 字节保留载荷。
    stderr_base64: Option<String>,
}

/// Captured bytes and any failure observed while draining one child process pipe.
/// 子进程单个管道读取过程中捕获的字节以及观察到的失败。
struct PipeCapture {
    /// Bytes captured before EOF or the first read failure.
    /// 在 EOF 或首次读取失败前已经捕获的字节。
    bytes: Vec<u8>,
    /// Explicit pipe read or reader-thread failure, if one occurred.
    /// 发生管道读取或读取线程失败时的显式错误。
    error: Option<String>,
}

/// Result emitted by the child process stdin writer thread.
/// 子进程 stdin 写入线程产出的结果。
struct StdinWriteResult {
    /// Explicit write or flush failure, if one occurred.
    /// 发生写入或 flush 失败时的显式错误。
    error: Option<String>,
}

/// Require a scalar text-like value for exec arguments and environment values.
/// 为 exec 的参数和环境变量值提取标量文本，拒绝 table/function 等复杂类型。
fn require_exec_scalar_text(
    value: LuaValue,
    fn_name: &str,
    param_name: &str,
    allow_blank: bool,
) -> mlua::Result<String> {
    match value {
        LuaValue::String(_) => require_string_arg(value, fn_name, param_name, allow_blank),
        LuaValue::Integer(number) => Ok(number.to_string()),
        LuaValue::Number(number) => {
            if !number.is_finite() {
                return Err(mlua::Error::runtime(format!(
                    "{fn_name}: {param_name} must be a finite number"
                )));
            }
            Ok(number.to_string())
        }
        LuaValue::Boolean(flag) => Ok(flag.to_string()),
        other => Err(mlua::Error::runtime(format!(
            "{fn_name}: {param_name} must be a string: {}",
            lua_value_type_name(&other)
        ))),
    }
}

/// Read an optional string field from a Lua table with strict validation.
/// 从 Lua table 中读取可选字符串字段，并执行严格校验。
fn table_get_optional_string_field(
    table: &Table,
    fn_name: &str,
    field_name: &str,
    allow_blank: bool,
) -> mlua::Result<Option<String>> {
    let value: LuaValue = table.get(field_name)?;
    match value {
        LuaValue::Nil => Ok(None),
        other => Ok(Some(require_string_arg(
            other,
            fn_name,
            field_name,
            allow_blank,
        )?)),
    }
}

/// Return the platform-default shell launcher used when Lua does not choose one explicitly.
/// 返回当 Lua 未显式选择时使用的平台默认 shell 启动器。
#[cfg(windows)]
fn default_exec_shell_launcher() -> ExecShellLauncher {
    ExecShellLauncher::Cmd
}

/// Return the platform-default shell launcher used when Lua does not choose one explicitly.
/// 返回当 Lua 未显式选择时使用的平台默认 shell 启动器。
#[cfg(not(windows))]
fn default_exec_shell_launcher() -> ExecShellLauncher {
    ExecShellLauncher::Sh
}

/// Return the stable default shell parameter name visible to Lua skills.
/// 返回对 Lua skill 可见的稳定默认 shell 参数名。
pub(super) fn default_exec_shell_name() -> &'static str {
    default_exec_shell_launcher().id()
}

/// Return the candidate shell launchers that the runtime may advertise on the current platform.
/// 返回运行时在当前平台上可能向外暴露的 shell 启动器候选集合。
#[cfg(windows)]
fn candidate_exec_shell_launchers() -> &'static [ExecShellLauncher] {
    &[
        ExecShellLauncher::Cmd,
        ExecShellLauncher::Pwsh,
        ExecShellLauncher::Powershell,
        ExecShellLauncher::Bash,
        ExecShellLauncher::Sh,
        ExecShellLauncher::Zsh,
    ]
}

/// Return the candidate shell launchers that the runtime may advertise on the current platform.
/// 返回运行时在当前平台上可能向外暴露的 shell 启动器候选集合。
#[cfg(not(windows))]
fn candidate_exec_shell_launchers() -> &'static [ExecShellLauncher] {
    &[
        ExecShellLauncher::Sh,
        ExecShellLauncher::Bash,
        ExecShellLauncher::Zsh,
        ExecShellLauncher::Pwsh,
        ExecShellLauncher::Powershell,
    ]
}

/// Check whether one shell launcher should be advertised as available on the current host.
/// 检查单个 shell 启动器是否应被标记为当前宿主可用。
fn is_exec_shell_launcher_available(launcher: ExecShellLauncher) -> bool {
    if launcher == default_exec_shell_launcher() {
        return true;
    }
    resolve_vulcan_process_which(launcher.program())
        .ok()
        .flatten()
        .is_some()
}

/// Resolve the concrete executable path used to spawn one shell launcher.
/// 解析启动单个 shell 启动器时应使用的实际可执行路径。
fn resolve_exec_shell_launcher_program(
    launcher: ExecShellLauncher,
) -> Result<std::ffi::OsString, String> {
    if launcher == default_exec_shell_launcher() {
        return Ok(std::ffi::OsString::from(launcher.program()));
    }
    match resolve_vulcan_process_which(launcher.program()) {
        Ok(Some(found)) => Ok(found.into_os_string()),
        Ok(None) => Err(format!(
            "process.exec: shell `{}` is not available in the current host",
            launcher.id()
        )),
        Err(error) => Err(format!(
            "process.exec: failed to resolve shell `{}`: {error}",
            launcher.id()
        )),
    }
}

/// Return the ordered shell parameter names supported by the current runtime host.
/// 返回当前运行时宿主支持的有序 shell 参数名列表。
pub(super) fn supported_exec_shell_names() -> Vec<&'static str> {
    let mut supported = Vec::new();
    for launcher in candidate_exec_shell_launchers().iter().copied() {
        if is_exec_shell_launcher_available(launcher) && !supported.contains(&launcher.id()) {
            supported.push(launcher.id());
        }
    }
    supported
}

/// Render the currently supported shell parameter names into one stable comma-separated string.
/// 将当前支持的 shell 参数名渲染为稳定的逗号分隔字符串。
fn render_supported_exec_shell_names() -> String {
    supported_exec_shell_names().join(", ")
}

/// Parse one normalized shell parameter value into its launcher descriptor.
/// 将单个规范化后的 shell 参数值解析为对应的启动器描述。
fn parse_exec_shell_launcher_id(value: &str) -> Option<ExecShellLauncher> {
    match value {
        "cmd" => Some(ExecShellLauncher::Cmd),
        "pwsh" => Some(ExecShellLauncher::Pwsh),
        "powershell" => Some(ExecShellLauncher::Powershell),
        "bash" => Some(ExecShellLauncher::Bash),
        "zsh" => Some(ExecShellLauncher::Zsh),
        "sh" => Some(ExecShellLauncher::Sh),
        _ => None,
    }
}

/// Resolve one Lua-provided `shell` string into one supported launcher or emit one actionable validation error.
/// 将 Lua 提供的 `shell` 字符串解析为受支持的启动器，或抛出可操作的校验错误。
fn resolve_exec_shell_launcher_from_label(
    label: &str,
    fn_name: &str,
) -> mlua::Result<ExecShellLauncher> {
    let normalized = label.trim().to_ascii_lowercase();
    let Some(launcher) = parse_exec_shell_launcher_id(&normalized) else {
        return Err(mlua::Error::runtime(format!(
            "{fn_name}: shell must be one of: {}",
            render_supported_exec_shell_names()
        )));
    };
    if !supported_exec_shell_names().contains(&launcher.id()) {
        return Err(mlua::Error::runtime(format!(
            "{fn_name}: shell `{}` is not available in the current host; available shell values: {}",
            launcher.id(),
            render_supported_exec_shell_names()
        )));
    }
    Ok(launcher)
}

/// Read one optional `shell` field that accepts either boolean compatibility flags or stable launcher names.
/// 读取可选的 `shell` 字段，该字段既接受兼容布尔值，也接受稳定的启动器名称。
fn table_get_optional_shell_field(
    table: &Table,
    fn_name: &str,
    field_name: &str,
) -> mlua::Result<Option<ExecShellSetting>> {
    let value: LuaValue = table.get(field_name)?;
    match value {
        LuaValue::Nil => Ok(None),
        LuaValue::Boolean(true) => Ok(Some(ExecShellSetting::UseDefault)),
        LuaValue::Boolean(false) => Ok(Some(ExecShellSetting::Disabled)),
        LuaValue::String(text) => {
            let shell_label = text.to_str().map_err(|_| {
                mlua::Error::runtime(format!(
                    "{fn_name}: {field_name} must be a valid UTF-8 string when provided"
                ))
            })?;
            Ok(Some(ExecShellSetting::Selected(
                resolve_exec_shell_launcher_from_label(shell_label.as_ref(), fn_name)?,
            )))
        }
        other => Err(mlua::Error::runtime(format!(
            "{fn_name}: {field_name} must be a boolean or string when provided: {}",
            lua_value_type_name(&other)
        ))),
    }
}

/// Read an optional timeout field in milliseconds from a Lua table.
/// 从 Lua table 中读取可选的毫秒级超时字段。
fn table_get_optional_timeout_field(
    table: &Table,
    fn_name: &str,
    field_name: &str,
) -> mlua::Result<Option<u64>> {
    let value: LuaValue = table.get(field_name)?;
    match value {
        LuaValue::Nil => Ok(None),
        LuaValue::Integer(number) if number > 0 => Ok(Some(number as u64)),
        LuaValue::Number(number) if number.is_finite() && number.fract() == 0.0 && number > 0.0 => {
            Ok(Some(number as u64))
        }
        other => Err(mlua::Error::runtime(format!(
            "{fn_name}: {field_name} must be a positive integer in milliseconds: {}",
            lua_value_type_name(&other)
        ))),
    }
}

/// Read an optional runtime text encoding field from a Lua table.
/// 从 Lua table 中读取可选的运行时文本编码字段。
fn table_get_optional_encoding_field(
    table: &Table,
    fn_name: &str,
    field_name: &str,
) -> mlua::Result<Option<RuntimeTextEncoding>> {
    let Some(label) = table_get_optional_string_field(table, fn_name, field_name, false)? else {
        return Ok(None);
    };
    RuntimeTextEncoding::parse(&label)
        .map(Some)
        .map_err(|error| mlua::Error::runtime(format!("{fn_name}: {field_name}: {error}")))
}

/// Read an optional string-like array field from a Lua table.
/// 从 Lua table 中读取可选的字符串类数组字段。
fn table_get_string_list_field(
    table: &Table,
    fn_name: &str,
    field_name: &str,
) -> mlua::Result<Vec<String>> {
    let value: LuaValue = table.get(field_name)?;
    match value {
        LuaValue::Nil => Ok(Vec::new()),
        other => {
            let list = require_table_arg(other, fn_name, field_name)?;
            let mut items = Vec::new();
            for (index, item) in list.sequence_values::<LuaValue>().enumerate() {
                let item = item.map_err(|error| {
                    mlua::Error::runtime(format!(
                        "{fn_name}: failed to read {field_name}[{}]: {}, {}",
                        index + 1,
                        index + 1,
                        error
                    ))
                })?;
                items.push(require_exec_scalar_text(
                    item,
                    fn_name,
                    &format!("{field_name}[{}]", index + 1),
                    true,
                )?);
            }
            Ok(items)
        }
    }
}

/// Read an optional string map field from a Lua table.
/// 从 Lua table 中读取可选的字符串映射字段。
fn table_get_string_map_field(
    table: &Table,
    fn_name: &str,
    field_name: &str,
) -> mlua::Result<HashMap<String, String>> {
    let value: LuaValue = table.get(field_name)?;
    match value {
        LuaValue::Nil => Ok(HashMap::new()),
        other => {
            let map_table = require_table_arg(other, fn_name, field_name)?;
            let mut items = HashMap::new();
            for pair in map_table.pairs::<LuaValue, LuaValue>() {
                let (key_value, field_value) = pair.map_err(|_error| {
                    mlua::Error::runtime(format!("{fn_name}: failed to read {field_name}"))
                })?;
                let key =
                    require_string_arg(key_value, fn_name, &format!("{field_name}.<key>"), false)?;
                let value_text = require_exec_scalar_text(
                    field_value,
                    fn_name,
                    &format!("{field_name}.{key}"),
                    true,
                )?;
                items.insert(key, value_text);
            }
            Ok(items)
        }
    }
}

/// Resolve the host-configured default runtime text encoding.
/// 解析宿主配置的默认运行时文本编码。
pub(super) fn resolve_host_default_text_encoding(
    host_options: &LuaRuntimeHostOptions,
) -> Result<RuntimeTextEncoding, String> {
    match host_options.default_text_encoding.as_deref() {
        Some(label) if !label.trim().is_empty() => RuntimeTextEncoding::parse(label),
        _ => Ok(default_runtime_text_encoding()),
    }
}

/// Parse Lua input into an executable process request.
/// 将 Lua 输入解析为可执行的进程请求。
pub(super) fn parse_exec_request(
    value: LuaValue,
    fn_name: &str,
    default_encoding: RuntimeTextEncoding,
) -> mlua::Result<ExecRequest> {
    match value {
        LuaValue::String(command_text) => Ok(ExecRequest {
            mode: ExecMode::Shell {
                command: require_string_arg(
                    LuaValue::String(command_text),
                    fn_name,
                    "command",
                    false,
                )?,
                launcher: default_exec_shell_launcher(),
            },
            cwd: None,
            env: HashMap::new(),
            stdin: None,
            timeout_ms: None,
            stdout_encoding: default_encoding,
            stderr_encoding: default_encoding,
            stdin_encoding: default_encoding,
        }),
        LuaValue::Table(spec) => {
            let command = table_get_optional_string_field(&spec, fn_name, "command", false)?;
            let program = table_get_optional_string_field(&spec, fn_name, "program", false)?;
            let args = table_get_string_list_field(&spec, fn_name, "args")?;
            let cwd = table_get_optional_string_field(&spec, fn_name, "cwd", false)?;
            let env = table_get_string_map_field(&spec, fn_name, "env")?;
            let stdin = table_get_optional_string_field(&spec, fn_name, "stdin", true)?;
            let timeout_ms = table_get_optional_timeout_field(&spec, fn_name, "timeout_ms")?;
            let shell_setting = table_get_optional_shell_field(&spec, fn_name, "shell")?;
            let encoding = table_get_optional_encoding_field(&spec, fn_name, "encoding")?
                .unwrap_or(default_encoding);
            let stdout_encoding =
                table_get_optional_encoding_field(&spec, fn_name, "stdout_encoding")?
                    .unwrap_or(encoding);
            let stderr_encoding =
                table_get_optional_encoding_field(&spec, fn_name, "stderr_encoding")?
                    .unwrap_or(encoding);
            let stdin_encoding =
                table_get_optional_encoding_field(&spec, fn_name, "stdin_encoding")?
                    .unwrap_or(encoding);

            if let Some(current_dir) = cwd.as_deref() {
                validate_path_text(current_dir, fn_name, "cwd")?;
            }

            let mode = match (command, program) {
                (Some(command_text), None) => {
                    if matches!(shell_setting, Some(ExecShellSetting::Disabled)) {
                        return Err(mlua::Error::runtime(format!(
                            "{fn_name}: shell=false cannot be used with command mode"
                        )));
                    }
                    if !args.is_empty() {
                        return Err(mlua::Error::runtime(format!(
                            "{fn_name}: args is only supported with program mode"
                        )));
                    }
                    let launcher = match shell_setting {
                        Some(ExecShellSetting::Selected(launcher)) => launcher,
                        _ => default_exec_shell_launcher(),
                    };
                    ExecMode::Shell {
                        command: command_text,
                        launcher,
                    }
                }
                (None, Some(program_path)) => {
                    match shell_setting {
                        Some(ExecShellSetting::UseDefault) => {
                            return Err(mlua::Error::runtime(format!(
                                "{fn_name}: shell=true requires command mode"
                            )));
                        }
                        Some(ExecShellSetting::Selected(launcher)) => {
                            return Err(mlua::Error::runtime(format!(
                                "{fn_name}: shell=\"{}\" requires command mode",
                                launcher.id()
                            )));
                        }
                        _ => {}
                    }
                    ExecMode::Program {
                        program: program_path,
                        args,
                    }
                }
                (Some(_), Some(_)) => {
                    return Err(mlua::Error::runtime(format!(
                        "{fn_name}: command and program are mutually exclusive"
                    )));
                }
                (None, None) => {
                    return Err(mlua::Error::runtime(format!(
                        "{fn_name}: expected a string command or a table with command"
                    )));
                }
            };

            Ok(ExecRequest {
                mode,
                cwd,
                env,
                stdin,
                timeout_ms,
                stdout_encoding,
                stderr_encoding,
                stdin_encoding,
            })
        }
        other => Err(mlua::Error::runtime(format!(
            "{fn_name}: expected a string or table, got {}",
            lua_value_type_name(&other)
        ))),
    }
}

/// Spawn a background reader for a named child process output pipe.
/// 为具名子进程输出管道启动后台读取线程。
fn spawn_pipe_reader<R>(stream_name: &'static str, mut reader: R) -> thread::JoinHandle<PipeCapture>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        // Buffer keeps partial output so capture failures do not discard already-read bytes.
        // 缓冲区保留部分输出，确保捕获失败不会丢弃已经读取到的字节。
        let mut buffer = Vec::new();
        // Read error is recorded instead of being converted into an empty output stream.
        // 读取错误会被记录下来，而不是被转换为空输出流。
        let read_error = reader
            .read_to_end(&mut buffer)
            .err()
            .map(|error| format!("failed to read process {}: {}", stream_name, error));
        PipeCapture {
            bytes: buffer,
            error: read_error,
        }
    })
}

/// Join one optional pipe reader and convert reader panics into explicit capture failures.
/// 等待一个可选管道读取线程，并将读取线程 panic 转换为显式捕获失败。
fn join_pipe_reader(
    handle: Option<thread::JoinHandle<PipeCapture>>,
    stream_name: &'static str,
) -> PipeCapture {
    match handle {
        Some(handle) => match handle.join() {
            Ok(capture) => capture,
            Err(_) => PipeCapture {
                bytes: Vec::new(),
                error: Some(format!("process {} reader thread panicked", stream_name)),
            },
        },
        None => PipeCapture {
            bytes: Vec::new(),
            error: None,
        },
    }
}

/// Spawn a background writer for a child process stdin pipe.
/// 为子进程标准输入管道启动后台写入线程。
fn spawn_stdin_writer<W>(mut writer: W, input: Vec<u8>) -> thread::JoinHandle<StdinWriteResult>
where
    W: Write + Send + 'static,
{
    thread::spawn(move || {
        if let Err(error) = writer.write_all(&input) {
            return StdinWriteResult {
                error: Some(format!("failed to write process stdin: {}", error)),
            };
        }
        if let Err(error) = writer.flush() {
            return StdinWriteResult {
                error: Some(format!("failed to flush process stdin: {}", error)),
            };
        }
        StdinWriteResult { error: None }
    })
}

/// Join one optional stdin writer and convert writer panics into explicit execution failures.
/// 等待一个可选 stdin 写入线程，并将写入线程 panic 转换为显式执行失败。
fn join_stdin_writer(handle: Option<thread::JoinHandle<StdinWriteResult>>) -> Option<String> {
    match handle {
        Some(handle) => match handle.join() {
            Ok(result) => result.error,
            Err(_) => Some("process stdin writer thread panicked".to_string()),
        },
        None => None,
    }
}

/// Build a structured process error result before stdout/stderr bytes are available.
/// 在 stdout/stderr 字节可用之前构建结构化进程错误结果。
fn exec_error_result(error_text: String, request: &ExecRequest, timed_out: bool) -> ExecResult {
    ExecResult {
        ok: false,
        success: false,
        code: None,
        stdout: String::new(),
        stderr: error_text.clone(),
        timed_out,
        error: Some(error_text),
        stdout_encoding: request.stdout_encoding.requested_label().to_string(),
        stderr_encoding: request.stderr_encoding.requested_label().to_string(),
        stdout_lossy: false,
        stderr_lossy: false,
        stdout_base64: None,
        stderr_base64: None,
    }
}

/// Execute a process request and capture its structured result.
/// 执行进程请求并捕获结构化结果。
pub(super) fn execute_exec_request(request: ExecRequest) -> ExecResult {
    let stdin_bytes = match request.stdin.as_deref() {
        Some(input) => match encode_runtime_text(input, request.stdin_encoding) {
            Ok(bytes) => Some(bytes),
            Err(error) => {
                let error_text = format!("failed to encode process stdin: {error}");
                return exec_error_result(error_text, &request, false);
            }
        },
        None => None,
    };

    let mut command = match &request.mode {
        ExecMode::Shell { command, launcher } => {
            let shell_program = match resolve_exec_shell_launcher_program(*launcher) {
                Ok(program) => program,
                Err(error_text) => return exec_error_result(error_text, &request, false),
            };
            let mut process = Command::new(shell_program);
            process.args(launcher.command_args(command));
            process
        }
        ExecMode::Program { program, args } => {
            let mut process = Command::new(program);
            process.args(args);
            process
        }
    };

    if let Some(current_dir) = &request.cwd {
        command.current_dir(current_dir);
    }
    if !request.env.is_empty() {
        command.envs(&request.env);
    }
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.stdin(if stdin_bytes.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let error_text = format!("failed to spawn process: {}", error);
            return exec_error_result(error_text, &request, false);
        }
    };

    let stdout_handle = child
        .stdout
        .take()
        .map(|stdout| spawn_pipe_reader("stdout", stdout));
    let stderr_handle = child
        .stderr
        .take()
        .map(|stderr| spawn_pipe_reader("stderr", stderr));
    let stdin_handle = match (stdin_bytes, child.stdin.take()) {
        (Some(input), Some(stdin)) => Some(spawn_stdin_writer(stdin, input)),
        _ => None,
    };

    // Timeout duration that actually triggered child termination, when a timeout occurs.
    // 实际触发子进程终止的超时时长；仅在发生超时时存在。
    let mut timed_out_after_ms = None;
    // Process start instant used to compare elapsed runtime with the requested timeout.
    // 用于将已运行时间与请求超时时长比较的进程启动时间点。
    let started_at = Instant::now();

    let final_status = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                break Some(status);
            }
            Ok(None) => {
                if let Some(timeout_ms) = request.timeout_ms
                    && started_at.elapsed() >= Duration::from_millis(timeout_ms)
                {
                    timed_out_after_ms = Some(timeout_ms);
                    let _ = child.kill();
                    break child.wait().ok();
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                let error_text = format!("failed to wait for process: {}", error);
                return exec_error_result(error_text, &request, timed_out_after_ms.is_some());
            }
        }
    };

    // Stdin write errors are execution-boundary failures when the caller requested stdin input.
    // 当调用方请求 stdin 输入时，stdin 写入错误属于执行边界失败。
    let stdin_error = join_stdin_writer(stdin_handle);

    // Stdout capture carries both partial bytes and an explicit capture error.
    // stdout 捕获结果同时携带部分字节与显式捕获错误。
    let PipeCapture {
        bytes: stdout_bytes,
        error: stdout_error,
    } = join_pipe_reader(stdout_handle, "stdout");
    // Stderr capture carries both partial bytes and an explicit capture error.
    // stderr 捕获结果同时携带部分字节与显式捕获错误。
    let PipeCapture {
        bytes: stderr_bytes,
        error: stderr_error,
    } = join_pipe_reader(stderr_handle, "stderr");
    // IO boundary errors are kept until bytes have been decoded for the final result envelope.
    // IO 边界错误会保留到字节解码完成后再写入最终结果包络。
    let capture_errors = [stdin_error, stdout_error, stderr_error]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let decoded_stdout = decode_runtime_text(&stdout_bytes, request.stdout_encoding);
    let decoded_stderr = decode_runtime_text(&stderr_bytes, request.stderr_encoding);
    let stdout = decoded_stdout.text;
    let mut stderr = decoded_stderr.text;

    // Whether this execution crossed the explicit timeout boundary.
    // 本次执行是否越过了显式超时边界。
    let timed_out = timed_out_after_ms.is_some();
    let status = match final_status {
        Some(status) => status,
        None => {
            let error_text = "process finished without status".to_string();
            return ExecResult {
                ok: false,
                success: false,
                code: None,
                stdout,
                stderr: error_text.clone(),
                timed_out,
                error: Some(error_text),
                stdout_encoding: decoded_stdout.encoding,
                stderr_encoding: decoded_stderr.encoding,
                stdout_lossy: decoded_stdout.lossy,
                stderr_lossy: decoded_stderr.lossy,
                stdout_base64: decoded_stdout.base64,
                stderr_base64: decoded_stderr.base64,
            };
        }
    };

    let code = status.code();
    // Process success only reflects timeout and exit status, not pipe capture health.
    // 进程自身成功只反映超时与退出状态，不混入管道捕获健康状态。
    let process_success = !timed_out && status.success();
    // Overall success also requires stdin/stdout/stderr IO boundaries to complete without errors.
    // 整体成功还要求 stdin/stdout/stderr IO 边界没有错误。
    let success = process_success && capture_errors.is_empty();
    let mut error = None;

    if let Some(timeout_value) = timed_out_after_ms {
        let timeout_text = format!("process execution timed out after {} ms", timeout_value);
        if !stderr.is_empty() {
            stderr.push('\n');
        }
        stderr.push_str(&timeout_text);
        error = Some(timeout_text);
    } else if !process_success {
        error = Some(match code {
            Some(exit_code) => format!("process exited with code {}", exit_code),
            None => "process terminated without an exit code".to_string(),
        });
    }
    if !capture_errors.is_empty() {
        // IO boundary errors are appended to stderr because they explain why the envelope failed.
        // IO 边界错误会追加到 stderr，因为它们解释了结果包络失败的原因。
        let capture_error_text = capture_errors.join("; ");
        if !stderr.is_empty() {
            stderr.push('\n');
        }
        stderr.push_str(&capture_error_text);
        error = Some(match error {
            Some(existing_error) => format!("{}; {}", existing_error, capture_error_text),
            None => capture_error_text,
        });
    }

    ExecResult {
        ok: success,
        success,
        code,
        stdout,
        stderr,
        timed_out,
        error,
        stdout_encoding: decoded_stdout.encoding,
        stderr_encoding: decoded_stderr.encoding,
        stdout_lossy: decoded_stdout.lossy,
        stderr_lossy: decoded_stderr.lossy,
        stdout_base64: decoded_stdout.base64,
        stderr_base64: decoded_stderr.base64,
    }
}

/// Convert an exec result into a Lua table for skill consumption.
/// 将 exec 结果转换为供 skill 消费的 Lua table。
pub(super) fn exec_result_to_lua_table(lua: &Lua, result: ExecResult) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set("ok", result.ok)?;
    table.set("success", result.success)?;
    table.set("stdout", result.stdout)?;
    table.set("stderr", result.stderr)?;
    table.set("stdout_encoding", result.stdout_encoding)?;
    table.set("stderr_encoding", result.stderr_encoding)?;
    table.set("stdout_lossy", result.stdout_lossy)?;
    table.set("stderr_lossy", result.stderr_lossy)?;
    match result.stdout_base64 {
        Some(stdout_base64) => table.set("stdout_base64", stdout_base64)?,
        None => table.set("stdout_base64", LuaValue::Nil)?,
    }
    match result.stderr_base64 {
        Some(stderr_base64) => table.set("stderr_base64", stderr_base64)?,
        None => table.set("stderr_base64", LuaValue::Nil)?,
    }
    table.set("timed_out", result.timed_out)?;
    match result.code {
        Some(code) => table.set("code", code)?,
        None => table.set("code", LuaValue::Nil)?,
    }
    match result.error {
        Some(error_text) => table.set("error", error_text)?,
        None => table.set("error", LuaValue::Nil)?,
    }
    Ok(table)
}

impl LuaEngine {
    /// Populate the `vulcan.runtime.lua.exec` bridge for normal skill VMs.
    /// 为普通 skill 虚拟机注入 `vulcan.runtime.lua.exec` 桥接函数。
    pub(super) fn populate_vulcan_luaexec_bridge(
        lua: &Lua,
        runtime_context: RunLuaRuntimeContext,
    ) -> Result<(), String> {
        let runtime_lua = get_vulcan_runtime_lua_table(lua)?;

        let exec_fn = lua
            .create_function(move |lua, input: LuaValue| {
                let input_table = require_table_arg(input, "runtime.lua.exec", "input")?;
                let input_json = lua_value_to_json(&LuaValue::Table(input_table))
                    .map_err(mlua::Error::runtime)?;
                let mut request: RunLuaExecRequest =
                    serde_json::from_value(input_json).map_err(|error| {
                        mlua::Error::runtime(format!("luaexec input is invalid: {}", error))
                    })?;
                let internal =
                    get_vulcan_runtime_internal_table(lua).map_err(mlua::Error::runtime)?;
                let caller_tool_name: Option<String> =
                    internal.get("tool_name").map_err(mlua::Error::runtime)?;
                request.caller_tool_name = caller_tool_name
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty());
                let rendered = LuaEngine::execute_runlua_request_inline_with_runtime(
                    &request,
                    runtime_context.clone(),
                )
                .map_err(mlua::Error::runtime)?;
                Ok(LuaValue::String(
                    lua.create_string(&rendered).map_err(mlua::Error::runtime)?,
                ))
            })
            .map_err(|error| format!("Failed to create vulcan.runtime.lua.exec: {}", error))?;
        runtime_lua
            .set("exec", exec_fn)
            .map_err(|error| format!("Failed to set vulcan.runtime.lua.exec: {}", error))?;
        Ok(())
    }

    /// Execute arbitrary Lua code inside one already selected VM lease.
    /// 在一个已经选定的虚拟机租约中执行任意 Lua 代码。
    fn run_lua_with_lease(
        &self,
        lease: &mut LuaVmLease,
        code: &str,
        args: &Value,
        invocation_context: Option<&LuaInvocationContext>,
    ) -> Result<Value, String> {
        let scope_guard = LuaVmRequestScopeGuard::new(lease, self.host_options.as_ref())?;
        let lua = scope_guard.lua()?;
        Self::populate_anonymous_lua_context(
            lua,
            AnonymousLuaExecutionContext {
                invocation_context,
                internal_context: VulcanInternalExecutionContext::default(),
                entry_file: None,
                dependency_context: AnonymousLuaDependencyContext::ClearWithHostOptions(
                    self.host_options.as_ref(),
                ),
            },
        )?;

        // Build a wrapper that passes args as a local variable.
        // 构造包装代码，将 args 作为局部变量传入 Lua 片段。
        let args_table = json_to_lua_table(lua, args)?;
        lua.globals()
            .set("__runlua_args", args_table)
            .map_err(|e| format!("Failed to set args: {}", e))?;

        let wrapper = format!(
            "return (function()\n  local args = __runlua_args\n  {}\nend)()",
            code
        );

        let run_result = (|| {
            let result = lua.load(&wrapper).eval::<LuaValue>().map_err(|e| {
                let msg = format!("Lua run_lua error: {}", e);
                log_error(format!("[LuaSkill:error] {}", msg));
                msg
            })?;

            lua_value_to_json(&result)
        })();
        finish_pooled_vm_request_scope(run_result, scope_guard, "pooled Lua VM cleanup failed")
    }

    /// Execute arbitrary Lua code against the current active runtime view and return the result.
    /// 针对当前已激活运行时视图执行任意 Lua 代码并返回结果。
    pub fn run_lua(
        &self,
        code: &str,
        args: &Value,
        invocation_context: Option<&LuaInvocationContext>,
    ) -> Result<Value, String> {
        let mut lease = self.acquire_vm()?;
        self.run_lua_with_lease(&mut lease, code, args, invocation_context)
    }

    /// Return the effective fixed `system_lua_lib` directory for the current engine.
    /// 返回当前引擎生效的固定 `system_lua_lib` 目录。
    fn acquire_runlua_vm(runtime_context: &RunLuaRuntimeContext) -> Result<LuaVmLease, String> {
        let runlua_pool = runtime_context.runlua_pool.clone();
        let runtime_context = runtime_context.clone();
        runlua_pool.acquire(move || {
            Self::create_runlua_vm(
                runtime_context.skills.as_ref(),
                runtime_context.entry_registry.as_ref(),
                runtime_context.host_options.clone(),
                runtime_context.skill_config_store.clone(),
                runtime_context.runtime_skill_roots.clone(),
                runtime_context.lancedb_host.clone(),
                runtime_context.sqlite_host.clone(),
            )
        })
    }

    /// Execute one isolated runlua request through the dedicated pooled runtime.
    /// 通过独立的池化运行时执行一次隔离 runlua 请求。
    fn execute_runlua_request_inline_with_runtime(
        request: &RunLuaExecRequest,
        runtime_context: RunLuaRuntimeContext,
    ) -> Result<String, String> {
        if request.timeout_ms == 0 {
            return Err("luaexec timeout_ms must be greater than 0".to_string());
        }
        let (resolved_code, entry_file) = Self::resolve_runlua_source(request)?;
        let mut lease = Self::acquire_runlua_vm(&runtime_context)?;
        let scope_guard =
            LuaVmRequestScopeGuard::new(&mut lease, runtime_context.host_options.as_ref())?;
        let lua = scope_guard.lua()?;
        let simulated_request_context = build_luaexec_call_request_context();
        let simulated_invocation_context = LuaInvocationContext::new(
            Some(simulated_request_context),
            Value::Object(serde_json::Map::new()),
            Value::Object(serde_json::Map::new()),
        );
        Self::populate_anonymous_lua_context(
            lua,
            AnonymousLuaExecutionContext {
                invocation_context: Some(&simulated_invocation_context),
                internal_context: VulcanInternalExecutionContext {
                    tool_name: None,
                    skill_name: None,
                    entry_name: None,
                    root_name: None,
                    luaexec_active: true,
                    luaexec_caller_tool_name: request.caller_tool_name.clone(),
                },
                entry_file: entry_file.as_deref(),
                // Preserve the cleared dependency context installed by the request scope reset.
                // 保留请求作用域 reset 已安装的清空依赖上下文。
                dependency_context: AnonymousLuaDependencyContext::PreserveCurrent,
            },
        )?;

        let captured_output: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        Self::configure_runlua_execution_environment(
            lua,
            captured_output.clone(),
            runtime_context.host_options.as_ref(),
        )?;

        let args_table = json_to_lua_table(lua, &request.args)?;
        lua.globals()
            .set("__runlua_args", args_table)
            .map_err(|error| format!("Failed to set runlua args: {}", error))?;

        let wrapper = format!(
            "return (function()\n  local args = __runlua_args\n  return table.pack((function()\n{}\nend)())\nend)()",
            resolved_code
        );

        Self::install_runlua_timeout_guard(lua, request.timeout_ms)
            .map_err(|error| error.to_string())?;
        let execution_result = Self::execute_runlua_wrapper(lua, &wrapper, entry_file.as_deref());
        Self::remove_runlua_timeout_guard(lua);
        let printed_output = lock_runlua_print_capture(&captured_output).clone();

        let render_result = match execution_result {
            Ok(returned_values) => {
                let rendered_values = Self::collect_runlua_return_values(&returned_values)?;
                Ok(Self::render_runlua_success_markdown(
                    request,
                    &printed_output,
                    &rendered_values,
                ))
            }
            Err(error) => Ok(Self::render_runlua_error_markdown(
                request,
                &printed_output,
                error.to_string().as_str(),
            )),
        };
        finish_pooled_vm_request_scope(
            render_result,
            scope_guard,
            "pooled runlua VM cleanup failed",
        )
    }

    /// Execute one isolated runlua request using the current engine snapshots.
    /// 使用当前引擎快照执行一次隔离 runlua 请求。
    fn execute_runlua_request_inline(&self, request: &RunLuaExecRequest) -> Result<String, String> {
        let runtime_context = RunLuaRuntimeContext::from_engine(
            self,
            Arc::new(self.skills.clone()),
            Arc::new(self.entry_registry.clone()),
        );
        Self::execute_runlua_request_inline_with_runtime(request, runtime_context)
    }

    /// Resolve one runlua request into concrete source text and optional entry file context.
    /// 将一次 runlua 请求解析成具体源代码文本及可选入口文件上下文。
    fn resolve_runlua_source(
        request: &RunLuaExecRequest,
    ) -> Result<(String, Option<PathBuf>), String> {
        let inline_code = request
            .code
            .as_ref()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string());
        let file_path = request
            .file
            .as_ref()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string());

        match (inline_code, file_path) {
            (Some(_), Some(_)) => {
                Err("luaexec accepts either code or file, but not both".to_string())
            }
            (None, None) => Err("luaexec requires code or file".to_string()),
            (Some(code), None) => Ok((code, None)),
            (None, Some(file_text)) => {
                validate_path_text(&file_text, "luaexec", "file")
                    .map_err(|error| error.to_string())?;
                let raw_file_path = PathBuf::from(&file_text);
                let file_path = if raw_file_path.is_absolute() {
                    raw_file_path
                } else {
                    std::env::current_dir()
                        .map_err(|error| {
                            format!("Failed to resolve luaexec relative file path: {}", error)
                        })?
                        .join(raw_file_path)
                };
                let source = std::fs::read_to_string(&file_path).map_err(|error| {
                    format!(
                        "Failed to read luaexec file {}: {}",
                        render_log_friendly_path(&file_path),
                        error
                    )
                })?;
                Ok((source, Some(file_path)))
            }
        }
    }

    /// Execute one inline runlua request from raw JSON text.
    /// 从原始 JSON 文本执行一次进程内 runlua 请求。
    pub fn execute_runlua_request_json_inline(&self, request_json: &str) -> Result<String, String> {
        let request: RunLuaExecRequest = serde_json::from_str(request_json)
            .map_err(|error| format!("Invalid luaexec request JSON: {}", error))?;
        self.execute_runlua_request_inline(&request)
    }

    /// Execute the runlua wrapper, optionally switching the process current directory to the entry file directory.
    /// 执行 runlua 包装器，并在需要时临时切换进程工作目录到入口文件目录。
    fn execute_runlua_wrapper(
        lua: &Lua,
        wrapper: &str,
        entry_file: Option<&Path>,
    ) -> Result<Table, mlua::Error> {
        match entry_file.and_then(Path::parent) {
            Some(entry_dir) => {
                let _cwd_guard = lock_runlua_cwd_guard();
                let original_dir = std::env::current_dir()
                    .map_err(|error| mlua::Error::runtime(format!("luaexec cwd: {}", error)))?;
                std::env::set_current_dir(entry_dir)
                    .map_err(|error| mlua::Error::runtime(format!("luaexec set cwd: {}", error)))?;
                let execution = lua.load(wrapper).eval::<Table>();
                let restore_result = std::env::set_current_dir(&original_dir).map_err(|error| {
                    mlua::Error::runtime(format!("luaexec restore cwd: {}", error))
                });
                match (execution, restore_result) {
                    (Ok(table), Ok(())) => Ok(table),
                    (Err(error), Ok(())) => Err(error),
                    (_, Err(error)) => Err(error),
                }
            }
            None => lua.load(wrapper).eval::<Table>(),
        }
    }

    /// Configure the isolated runlua execution VM.
    /// 配置隔离 runlua 执行虚拟机的运行时环境。
    fn configure_runlua_execution_environment(
        lua: &Lua,
        captured_output: Arc<Mutex<Vec<String>>>,
        host_options: &LuaRuntimeHostOptions,
    ) -> Result<(), String> {
        let runtime = get_vulcan_runtime_table(lua)?;
        let runtime_lua = get_vulcan_runtime_lua_table(lua)?;
        let vulcan = get_vulcan_table(lua)?;
        let cache = vulcan
            .get::<Table>("cache")
            .map_err(|error| format!("Failed to get vulcan.cache: {}", error))?;
        let vulcan_io = vulcan
            .get::<Table>("io")
            .map_err(|error| format!("Failed to get vulcan.io: {}", error))?;

        let print_capture = captured_output.clone();
        let print_fn = lua
            .create_function(move |_, args: MultiValue| {
                let mut parts = Vec::new();
                for value in args.into_iter() {
                    parts.push(LuaEngine::render_lua_value_inline(&value));
                }
                let mut guard = lock_runlua_print_capture(&print_capture);
                guard.push(parts.join("\t"));
                Ok(())
            })
            .map_err(|error| format!("Failed to create runlua print capture: {}", error))?;
        lua.globals()
            .set("print", print_fn)
            .map_err(|error| format!("Failed to override global print for runlua: {}", error))?;

        lua.load(
            r#"
if jit and type(jit.off) == "function" then
    jit.off(true, true)
end
if jit and type(jit.flush) == "function" then
    jit.flush()
end
"#,
        )
        .exec()
        .map_err(|error| format!("Failed to disable JIT for runlua: {}", error))?;

        runtime
            .set("log", LuaValue::Nil)
            .map_err(|error| format!("Failed to clear vulcan.runtime.log for runlua: {}", error))?;
        cache
            .set("put", LuaValue::Nil)
            .map_err(|error| format!("Failed to clear vulcan.cache.put for runlua: {}", error))?;
        cache
            .set("get", LuaValue::Nil)
            .map_err(|error| format!("Failed to clear vulcan.cache.get for runlua: {}", error))?;
        cache.set("delete", LuaValue::Nil).map_err(|error| {
            format!("Failed to clear vulcan.cache.delete for runlua: {}", error)
        })?;
        runtime_lua.set("exec", LuaValue::Nil).map_err(|error| {
            format!(
                "Failed to clear vulcan.runtime.lua.exec for runlua: {}",
                error
            )
        })?;
        if host_options.capabilities.enable_managed_io_compat {
            let default_encoding = resolve_host_default_text_encoding(host_options)?;
            install_managed_io_compat(lua, &vulcan_io, default_encoding).map_err(|error| {
                format!(
                    "Failed to install managed io compatibility for runlua: {}",
                    error
                )
            })?;
        }
        Ok(())
    }

    /// Install a hard timeout guard for the isolated luaexec VM.
    /// 为隔离 luaexec 虚拟机安装硬超时保护。
    pub(super) fn install_runlua_timeout_guard(lua: &Lua, timeout_ms: u64) -> mlua::Result<()> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let timeout_text = format!("luaexec execution timed out after {} ms", timeout_ms);

        lua.set_hook(
            HookTriggers::new().every_nth_instruction(1_000),
            move |_, _| {
                if Instant::now() >= deadline {
                    return Err(mlua::Error::runtime(timeout_text.clone()));
                }
                Ok(VmState::Continue)
            },
        )
    }

    /// Remove the previously installed timeout guard from the isolated luaexec VM.
    /// 移除隔离 luaexec 虚拟机上已安装的超时保护。
    pub(super) fn remove_runlua_timeout_guard(lua: &Lua) {
        lua.remove_hook();
    }

    /// Collect packed Lua return values from the isolated runlua wrapper.
    /// 从隔离 runlua 包装器返回的打包结果中提取所有返回值。
    fn collect_runlua_return_values(
        result_table: &Table,
    ) -> Result<Vec<RunLuaRenderedValue>, String> {
        let value_count = result_table
            .get::<i64>("n")
            .map_err(|error| format!("Failed to read runlua return count: {}", error))?
            .max(0) as usize;

        let mut rendered_values = Vec::new();
        if value_count == 0 {
            rendered_values.push(RunLuaRenderedValue {
                format: "json",
                content: "null".to_string(),
            });
            return Ok(rendered_values);
        }

        for index in 1..=value_count {
            let value: LuaValue = result_table.raw_get(index).map_err(|error| {
                format!("Failed to read runlua return value {}: {}", index, error)
            })?;
            rendered_values.push(Self::render_runlua_value(&value));
        }

        Ok(rendered_values)
    }

    /// Render one Lua return value into a Markdown-ready block payload.
    /// 将单个 Lua 返回值渲染为可直接写入 Markdown 代码块的载荷。
    fn render_runlua_value(value: &LuaValue) -> RunLuaRenderedValue {
        match value {
            LuaValue::String(text) => RunLuaRenderedValue {
                format: "text",
                content: render_lua_print_argument(LuaValue::String(text.clone())),
            },
            _ => match lua_value_to_json(value) {
                Ok(json_value) => RunLuaRenderedValue {
                    format: "json",
                    content: serde_json::to_string_pretty(&json_value)
                        .unwrap_or_else(|_| "null".to_string()),
                },
                Err(_) => RunLuaRenderedValue {
                    format: "text",
                    content: Self::render_lua_value_inline(value),
                },
            },
        }
    }

    /// Render one Lua value into a compact single-line textual form.
    /// 将单个 Lua 值渲染为紧凑的单行文本形式。
    fn render_lua_value_inline(value: &LuaValue) -> String {
        render_lua_print_argument(value.clone())
    }

    /// Render a successful runlua execution result into Markdown text.
    /// 将成功的 runlua 执行结果渲染为 Markdown 文本。
    fn render_runlua_success_markdown(
        request: &RunLuaExecRequest,
        printed_output: &[String],
        rendered_values: &[RunLuaRenderedValue],
    ) -> String {
        let mut lines = vec![
            "# Runtime Execution Result".to_string(),
            "".to_string(),
            "## Task".to_string(),
            if request.task.trim().is_empty() {
                "Execute Lua runtime code".to_string()
            } else {
                request.task.trim().to_string()
            },
            "".to_string(),
            "## Status".to_string(),
            "SUCCESS".to_string(),
        ];

        if !printed_output.is_empty() {
            lines.extend([
                "".to_string(),
                "## Printed Output".to_string(),
                "```text".to_string(),
                printed_output.join("\n"),
                "```".to_string(),
            ]);
        }

        lines.extend(["".to_string(), "## Returned Values".to_string()]);

        for (index, value) in rendered_values.iter().enumerate() {
            lines.push(format!("{}. ", index + 1));
            lines.push(format!("```{}", value.format));
            lines.push(value.content.clone());
            lines.push("```".to_string());
            if index + 1 < rendered_values.len() {
                lines.push("".to_string());
            }
        }

        lines.join("\n")
    }

    /// Render a failed runlua execution result into Markdown text.
    /// 将失败的 runlua 执行结果渲染为 Markdown 文本。
    fn render_runlua_error_markdown(
        request: &RunLuaExecRequest,
        printed_output: &[String],
        error_text: &str,
    ) -> String {
        let mut lines = vec![
            "# Runtime Execution Error".to_string(),
            "".to_string(),
            "## Task".to_string(),
            if request.task.trim().is_empty() {
                "Execute Lua runtime code".to_string()
            } else {
                request.task.trim().to_string()
            },
            "".to_string(),
            "## Status".to_string(),
            "FAILED".to_string(),
            "".to_string(),
            "## Error".to_string(),
            "```text".to_string(),
            error_text.to_string(),
            "```".to_string(),
        ];

        if !printed_output.is_empty() {
            lines.extend([
                "".to_string(),
                "## Printed Output".to_string(),
                "```text".to_string(),
                printed_output.join("\n"),
                "```".to_string(),
            ]);
        }

        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Read, Write};

    /// Reader fixture that emits one partial chunk and then fails.
    /// 先产出一个部分数据块然后失败的读取器夹具。
    struct FailingPipeReader {
        /// Whether the partial chunk has already been emitted.
        /// 部分数据块是否已经产出。
        emitted_partial_chunk: bool,
    }

    impl Read for FailingPipeReader {
        /// Read one partial chunk before returning a deterministic failure.
        /// 返回确定性失败前读取一个部分数据块。
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if !self.emitted_partial_chunk {
                // Partial bytes that must survive the later read failure.
                // 后续读取失败后仍必须保留下来的部分字节。
                let partial_bytes = b"partial-output";
                buffer[..partial_bytes.len()].copy_from_slice(partial_bytes);
                self.emitted_partial_chunk = true;
                return Ok(partial_bytes.len());
            }
            Err(io::Error::other("forced read failure"))
        }
    }

    /// Stdin writer fixture that fails on the first write attempt.
    /// 首次写入时失败的 stdin 写入器夹具。
    struct FailingStdinWrite;

    impl Write for FailingStdinWrite {
        /// Fail every stdin write attempt deterministically.
        /// 以确定性方式让每次 stdin 写入尝试失败。
        fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("forced stdin write failure"))
        }

        /// Flush is unreachable after a write failure but remains a valid trait implementation.
        /// 写入失败后不会触达 flush，但仍提供有效的 trait 实现。
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Stdin writer fixture that accepts writes and fails during flush.
    /// 接受写入但在 flush 阶段失败的 stdin 写入器夹具。
    struct FailingStdinFlush;

    impl Write for FailingStdinFlush {
        /// Accept the full stdin buffer so the writer reaches flush.
        /// 接受完整 stdin 缓冲区以便写入器进入 flush 阶段。
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            Ok(buffer.len())
        }

        /// Fail flush deterministically after the write succeeds.
        /// 在写入成功后以确定性方式让 flush 失败。
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("forced stdin flush failure"))
        }
    }

    /// Verify pipe read failures preserve partial bytes and report the capture error.
    /// 验证管道读取失败会保留部分字节并报告捕获错误。
    #[test]
    fn pipe_reader_reports_read_error_without_dropping_partial_bytes() {
        // Reader fixture that deterministically fails after one successful read.
        // 在一次成功读取后确定性失败的读取器夹具。
        let reader = FailingPipeReader {
            emitted_partial_chunk: false,
        };
        // Joined capture returned by the background pipe reader.
        // 后台管道读取器返回的已等待捕获结果。
        let capture = spawn_pipe_reader("stdout", reader)
            .join()
            .expect("pipe reader thread should not panic");

        assert_eq!(capture.bytes, b"partial-output");
        assert_eq!(
            capture.error.as_deref(),
            Some("failed to read process stdout: forced read failure")
        );
    }

    /// Verify pipe reader panics are returned as explicit capture errors.
    /// 验证管道读取线程 panic 会作为显式捕获错误返回。
    #[test]
    fn pipe_reader_join_reports_reader_thread_panic() {
        // Panicking reader handle that mimics an unexpected reader-thread failure.
        // 模拟读取线程意外失败的 panic 读取句柄。
        let handle = thread::spawn(|| -> PipeCapture { panic!("forced reader panic") });
        // Joined capture after converting the panic into a structured error.
        // 将 panic 转换为结构化错误后的已等待捕获结果。
        let capture = join_pipe_reader(Some(handle), "stderr");

        assert!(capture.bytes.is_empty());
        assert_eq!(
            capture.error.as_deref(),
            Some("process stderr reader thread panicked")
        );
    }

    /// Verify stdin write failures are returned as explicit execution errors.
    /// 验证 stdin 写入失败会作为显式执行错误返回。
    #[test]
    fn stdin_writer_reports_write_error() {
        // Writer fixture that rejects the first stdin write.
        // 拒绝首次 stdin 写入的写入器夹具。
        let writer = FailingStdinWrite;
        // Joined stdin writer result carrying the write failure.
        // 携带写入失败的已等待 stdin 写入结果。
        let result = spawn_stdin_writer(writer, b"payload".to_vec())
            .join()
            .expect("stdin writer thread should not panic");

        assert_eq!(
            result.error.as_deref(),
            Some("failed to write process stdin: forced stdin write failure")
        );
    }

    /// Verify stdin flush failures are returned as explicit execution errors.
    /// 验证 stdin flush 失败会作为显式执行错误返回。
    #[test]
    fn stdin_writer_reports_flush_error() {
        // Writer fixture that accepts writes and then fails during flush.
        // 接受写入并在 flush 阶段失败的写入器夹具。
        let writer = FailingStdinFlush;
        // Joined stdin writer result carrying the flush failure.
        // 携带 flush 失败的已等待 stdin 写入结果。
        let result = spawn_stdin_writer(writer, b"payload".to_vec())
            .join()
            .expect("stdin writer thread should not panic");

        assert_eq!(
            result.error.as_deref(),
            Some("failed to flush process stdin: forced stdin flush failure")
        );
    }

    /// Verify stdin writer panics are returned as explicit execution errors.
    /// 验证 stdin 写入线程 panic 会作为显式执行错误返回。
    #[test]
    fn stdin_writer_join_reports_writer_thread_panic() {
        // Panicking writer handle that mimics an unexpected stdin writer failure.
        // 模拟 stdin 写入线程意外失败的 panic 写入句柄。
        let handle = thread::spawn(|| -> StdinWriteResult { panic!("forced stdin writer panic") });
        // Structured error returned by the stdin writer join helper.
        // stdin 写入 join 辅助函数返回的结构化错误。
        let error = join_stdin_writer(Some(handle));

        assert_eq!(
            error.as_deref(),
            Some("process stdin writer thread panicked")
        );
    }
}
