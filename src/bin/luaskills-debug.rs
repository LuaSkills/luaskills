use luaskills::lua_skill::validate_luaskills_identifier;
use luaskills::runtime::render_host_visible_path;
use luaskills::{
    LuaEngine, LuaEngineOptions, LuaInvocationContext, LuaRuntimeHostOptions, LuaVmPoolConfig,
    RuntimeEntryDescriptor, RuntimeInvocationResult, RuntimeRequestContext, RuntimeSkillRoot,
    SkillMeta,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Default help text rendered by the standalone LuaSkills debug binary.
/// 独立 LuaSkills 调试二进制程序输出的默认帮助文本。
const DEBUG_USAGE: &str = r#"luaskills-debug

Usage:
  luaskills-debug sync --runtime-root <dir> --skill-path <dir> [--output pretty|json]
  luaskills-debug inspect --runtime-root <dir> --skill-path <dir> [--output pretty|json]
  luaskills-debug inspect --runtime-root <dir> --skill-id <id> [--output pretty|json]
  luaskills-debug list-tools --runtime-root <dir> --skill-path <dir> [--output pretty|json|content]
  luaskills-debug list-tools --runtime-root <dir> --skill-id <id> [--output pretty|json|content]
  luaskills-debug call --runtime-root <dir> --skill-path <dir> --tool <name> [--args-json <json> | --args-file <path>] [--enable-host-result] [--output pretty|json|content]
  luaskills-debug call --runtime-root <dir> --skill-id <id> --tool <name> [--args-json <json> | --args-file <path>] [--enable-host-result] [--output pretty|json|content]

Examples:
  luaskills-debug sync --runtime-root D:\runtime --skill-path D:\skills\vulcan-file
  luaskills-debug inspect --runtime-root D:\runtime --skill-path D:\skills\vulcan-file
  luaskills-debug inspect --runtime-root D:\runtime --skill-id vulcan-file
  luaskills-debug list-tools --runtime-root D:\runtime --skill-path D:\skills\vulcan-file --output content
  luaskills-debug call --runtime-root D:\runtime --skill-id vulcan-file --tool read --args-json "{\"path\":\"D:/demo.txt\"}"
"#;

/// Supported top-level debug subcommands.
/// 支持的顶层调试子命令集合。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DebugCommandKind {
    /// Synchronize one source skill into the debug runtime without loading or calling it.
    /// 仅将一个源 skill 同步到调试运行时，不执行加载或调用。
    Sync,
    /// Inspect one skill manifest and its loaded runtime entry mapping.
    /// 检查单个 skill 清单及其加载后的运行时入口映射。
    Inspect,
    /// List all callable tools exposed by the current debug skill.
    /// 列出当前调试 skill 对外暴露的全部可调用工具。
    ListTools,
    /// Call one tool entry of the current debug skill.
    /// 调用当前调试 skill 的单个工具入口。
    Call,
}

impl DebugCommandKind {
    /// Parse one command name from CLI text.
    /// 从命令行文本解析单个命令名称。
    fn parse(value: &str) -> Result<Self, String> {
        match value.trim() {
            "sync" => Ok(Self::Sync),
            "inspect" => Ok(Self::Inspect),
            "list-tools" => Ok(Self::ListTools),
            "call" => Ok(Self::Call),
            other => Err(format!("Unknown command '{}'", other)),
        }
    }
}

/// Supported output rendering modes of the debug binary.
/// 调试二进制程序支持的输出渲染模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DebugOutputMode {
    /// Human-readable multi-line output.
    /// 面向人的多行可读输出。
    Pretty,
    /// Structured JSON output.
    /// 结构化 JSON 输出。
    Json,
    /// Minimal content-only output.
    /// 最小化的纯内容输出。
    Content,
}

impl DebugOutputMode {
    /// Parse one output-mode string from CLI text.
    /// 从命令行文本解析单个输出模式字符串。
    fn parse(value: &str) -> Result<Self, String> {
        match value.trim() {
            "pretty" => Ok(Self::Pretty),
            "json" => Ok(Self::Json),
            "content" => Ok(Self::Content),
            other => Err(format!("Unsupported output mode '{}'", other)),
        }
    }
}

/// Parsed CLI command payload used by the debug binary.
/// 调试二进制程序使用的已解析命令载荷。
#[derive(Debug, Clone, PartialEq, Eq)]
struct DebugCliCommand {
    /// Selected debug subcommand.
    /// 当前选择的调试子命令。
    kind: DebugCommandKind,
    /// Effective runtime root used to host the synchronized debug skill.
    /// 用于承载同步后调试 skill 的运行时根目录。
    runtime_root: PathBuf,
    /// Source skill package directory supplied by the developer.
    /// 开发者传入的源 skill 包目录。
    skill_path: Option<PathBuf>,
    /// Effective skill identifier loaded from the runtime root when no source path is supplied.
    /// 未提供源路径时从运行时根目录加载的生效 skill 标识符。
    skill_id: Option<String>,
    /// Requested tool name for `call`.
    /// `call` 命令请求的工具名称。
    tool_name: Option<String>,
    /// Inline JSON text used as invocation args.
    /// 作为调用参数使用的内联 JSON 文本。
    args_json: Option<String>,
    /// JSON file path used as invocation args.
    /// 作为调用参数使用的 JSON 文件路径。
    args_file: Option<PathBuf>,
    /// Whether the host_result bridge should be explicitly enabled for this invocation.
    /// 当前调用是否应显式开启 host_result 桥接。
    enable_host_result: bool,
    /// Selected output mode.
    /// 当前选择的输出模式。
    output_mode: DebugOutputMode,
}

/// Prepared debug runtime state after skill synchronization and engine loading.
/// 在 skill 同步和引擎加载完成后的调试运行时准备状态。
struct PreparedDebugRuntime {
    /// Loaded LuaSkills runtime engine.
    /// 已加载完成的 LuaSkills 运行时引擎。
    engine: LuaEngine,
    /// Parsed and directory-bound skill manifest.
    /// 已解析并绑定目录 skill_id 的 skill 清单。
    manifest: SkillMeta,
    /// Stable skill identifier derived from the physical directory name.
    /// 从物理目录名称派生出的稳定 skill 标识符。
    skill_id: String,
    /// Absolute runtime root used during the debug run.
    /// 当前调试运行使用的绝对运行时根目录。
    runtime_root: PathBuf,
    /// Absolute original source skill directory.
    /// 原始源 skill 目录的绝对路径。
    source_skill_path: Option<PathBuf>,
    /// Absolute synchronized target skill directory under the runtime root.
    /// 运行时根目录下同步后的目标 skill 目录绝对路径。
    synced_skill_path: PathBuf,
    /// All loaded runtime entry descriptors belonging to the target skill.
    /// 属于目标 skill 的全部已加载运行时入口描述。
    entries: Vec<RuntimeEntryDescriptor>,
}

/// Internal result of synchronizing one source skill into a debug runtime root.
/// 将单个源 skill 同步进调试运行时根目录后的内部结果。
struct DebugSkillSyncResult {
    /// Effective bound skill identifier.
    /// 绑定后的生效 skill 标识符。
    skill_id: String,
    /// Absolute runtime root used by the debug command.
    /// 调试命令使用的绝对运行时根目录。
    runtime_root: PathBuf,
    /// Absolute source skill directory path.
    /// 源 skill 目录绝对路径。
    source_skill_path: PathBuf,
    /// Absolute synchronized skill directory path under the runtime root.
    /// runtime_root 下同步后的 skill 目录绝对路径。
    synced_skill_path: PathBuf,
}

/// Result of synchronizing one source skill into a debug runtime root.
/// 将单个源 skill 同步到调试运行时根目录后的结果。
#[derive(Debug, Serialize)]
struct DebugSyncOutput {
    /// Debug command name.
    /// 调试命令名称。
    command: &'static str,
    /// Effective bound skill identifier.
    /// 绑定后的生效 skill 标识符。
    skill_id: String,
    /// Absolute runtime root used by the debug command.
    /// 调试命令使用的绝对运行时根目录。
    runtime_root: String,
    /// Absolute source skill directory path.
    /// 源 skill 目录绝对路径。
    source_skill_path: String,
    /// Absolute synchronized skill directory path under the runtime root.
    /// runtime_root 下同步后的 skill 目录绝对路径。
    synced_skill_path: String,
}

/// Structured inspect output returned by the debug binary.
/// 调试二进制程序返回的结构化 inspect 输出。
#[derive(Debug, Serialize)]
struct DebugInspectOutput {
    /// Debug command name.
    /// 调试命令名称。
    command: &'static str,
    /// Effective bound skill identifier.
    /// 绑定后的生效 skill 标识符。
    skill_id: String,
    /// Manifest-declared package name.
    /// 清单中声明的包名称。
    manifest_name: String,
    /// Manifest-declared semantic version.
    /// 清单中声明的语义版本号。
    manifest_version: String,
    /// Whether the manifest enables debug hot-reload mode.
    /// 清单是否开启 debug 热加载模式。
    debug: bool,
    /// Absolute runtime root used by the debug command.
    /// 调试命令使用的绝对运行时根目录。
    runtime_root: String,
    /// Absolute source skill directory path.
    /// 源 skill 目录绝对路径。
    source_skill_path: Option<String>,
    /// Absolute synchronized skill directory path under the runtime root.
    /// runtime_root 下同步后的 skill 目录绝对路径。
    synced_skill_path: String,
    /// Loaded runtime entries of the current skill.
    /// 当前 skill 的已加载运行时入口集合。
    entries: Vec<RuntimeEntryDescriptor>,
}

/// Structured call output returned by the debug binary.
/// 调试二进制程序返回的结构化调用输出。
#[derive(Debug, Serialize)]
struct DebugCallOutput {
    /// Debug command name.
    /// 调试命令名称。
    command: &'static str,
    /// Effective bound skill identifier.
    /// 绑定后的生效 skill 标识符。
    skill_id: String,
    /// Original tool name requested by the developer.
    /// 开发者原始请求的工具名称。
    requested_tool_name: String,
    /// Canonical runtime tool name actually executed by the engine.
    /// 引擎实际执行的 canonical 运行时工具名称。
    resolved_tool_name: String,
    /// Absolute runtime root used by the debug command.
    /// 调试命令使用的绝对运行时根目录。
    runtime_root: String,
    /// Absolute synchronized skill directory path under the runtime root.
    /// runtime_root 下同步后的 skill 目录绝对路径。
    synced_skill_path: String,
    /// Invocation result returned by the runtime engine.
    /// 运行时引擎返回的调用结果。
    result: RuntimeInvocationResult,
}

/// Entry point of the standalone LuaSkills debug binary.
/// 独立 LuaSkills 调试二进制程序的入口点。
fn main() {
    match run_debug_binary() {
        Ok(()) => {}
        Err(error) => {
            eprintln!("luaskills-debug: {}", error);
            std::process::exit(1);
        }
    }
}

/// Execute the debug binary flow from CLI parsing to command dispatch.
/// 从命令行解析到命令分发，执行调试二进制程序的完整流程。
fn run_debug_binary() -> Result<(), String> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print!("{}", DEBUG_USAGE);
        return Ok(());
    }

    let command = parse_debug_cli(&args)?;
    match command.kind {
        DebugCommandKind::Sync => {
            let sync_result = sync_debug_skill(&command)?;
            let output = build_sync_output(&sync_result);
            render_sync_output(command.output_mode, &output)
        }
        DebugCommandKind::Inspect => {
            let prepared = prepare_debug_runtime(&command)?;
            let output = build_inspect_output(&prepared);
            render_inspect_output(command.output_mode, &output)
        }
        DebugCommandKind::ListTools => {
            let prepared = prepare_debug_runtime(&command)?;
            render_list_tools_output(command.output_mode, &prepared.entries)
        }
        DebugCommandKind::Call => {
            let prepared = prepare_debug_runtime(&command)?;
            let requested_tool_name = command
                .tool_name
                .clone()
                .ok_or_else(|| "call requires --tool".to_string())?;
            let resolved_tool_name =
                resolve_debug_tool_name(&prepared.entries, &requested_tool_name)?;
            let args_value = load_invocation_args(&command)?;
            let invocation_context = build_debug_invocation_context(command.enable_host_result);
            let result = prepared.engine.call_skill(
                &resolved_tool_name,
                &args_value,
                Some(&invocation_context),
            )?;
            let output = DebugCallOutput {
                command: "call",
                skill_id: prepared.skill_id.clone(),
                requested_tool_name,
                resolved_tool_name,
                runtime_root: render_host_visible_path(&prepared.runtime_root),
                synced_skill_path: render_host_visible_path(&prepared.synced_skill_path),
                result,
            };
            render_call_output(command.output_mode, &output)
        }
    }
}

/// Parse one CLI argument vector into a structured debug command.
/// 将一组命令行参数解析为结构化调试命令。
fn parse_debug_cli(args: &[String]) -> Result<DebugCliCommand, String> {
    let kind = DebugCommandKind::parse(
        args.first()
            .ok_or_else(|| "Missing debug subcommand".to_string())?,
    )?;

    let mut runtime_root: Option<PathBuf> = None;
    let mut skill_path: Option<PathBuf> = None;
    let mut skill_id: Option<String> = None;
    let mut tool_name: Option<String> = None;
    let mut args_json: Option<String> = None;
    let mut args_file: Option<PathBuf> = None;
    let mut enable_host_result = false;
    let mut output_mode = DebugOutputMode::Pretty;

    let mut index = 1usize;
    while index < args.len() {
        let flag = args[index].as_str();
        match flag {
            "--runtime-root" => {
                runtime_root = Some(PathBuf::from(read_cli_value(args, &mut index, flag)?));
            }
            "--skill-path" => {
                skill_path = Some(PathBuf::from(read_cli_value(args, &mut index, flag)?));
            }
            "--skill-id" => {
                skill_id = Some(read_cli_value(args, &mut index, flag)?.to_string());
            }
            "--tool" => {
                tool_name = Some(read_cli_value(args, &mut index, flag)?.to_string());
            }
            "--args-json" => {
                args_json = Some(read_cli_value(args, &mut index, flag)?.to_string());
            }
            "--args-file" => {
                args_file = Some(PathBuf::from(read_cli_value(args, &mut index, flag)?));
            }
            "--output" => {
                output_mode = DebugOutputMode::parse(read_cli_value(args, &mut index, flag)?)?;
            }
            "--enable-host-result" => {
                enable_host_result = true;
            }
            other => {
                return Err(format!("Unknown option '{}'", other));
            }
        }
        index += 1;
    }

    let runtime_root = runtime_root.ok_or_else(|| "--runtime-root is required".to_string())?;
    if args_json.is_some() && args_file.is_some() {
        return Err("--args-json and --args-file are mutually exclusive".to_string());
    }
    if skill_path.is_some() && skill_id.is_some() {
        return Err("--skill-path and --skill-id are mutually exclusive".to_string());
    }
    if kind == DebugCommandKind::Sync && skill_path.is_none() {
        return Err("sync requires --skill-path".to_string());
    }
    if kind != DebugCommandKind::Sync && skill_path.is_none() && skill_id.is_none() {
        return Err(format!("{} requires --skill-id or --skill-path", args[0]));
    }
    if let Some(skill_id) = skill_id.as_deref() {
        validate_luaskills_identifier(skill_id, "skill_id")?;
    }

    if kind == DebugCommandKind::Call && tool_name.is_none() {
        return Err("call requires --tool".to_string());
    }

    Ok(DebugCliCommand {
        kind,
        runtime_root,
        skill_path,
        skill_id,
        tool_name,
        args_json,
        args_file,
        enable_host_result,
        output_mode,
    })
}

/// Read one option value following a CLI flag and advance the parsing cursor.
/// 读取单个命令行选项标志后的值，并推进解析游标。
fn read_cli_value<'a>(
    args: &'a [String],
    index: &mut usize,
    flag: &str,
) -> Result<&'a str, String> {
    *index += 1;
    args.get(*index)
        .map(|value| value.as_str())
        .ok_or_else(|| format!("{} requires a value", flag))
}

/// Synchronize the source skill into the debug runtime root and return its stable location.
/// 将源 skill 同步到调试运行时根目录，并返回其稳定位置。
fn sync_debug_skill(command: &DebugCliCommand) -> Result<DebugSkillSyncResult, String> {
    let runtime_root = absolutize_path(&command.runtime_root)?;
    let source_skill_path = command
        .skill_path
        .as_ref()
        .ok_or_else(|| "sync requires --skill-path".to_string())
        .and_then(|path| absolutize_path(path))?;
    let mut manifest = load_bound_skill_manifest(&source_skill_path)?;
    let skill_id = manifest.effective_skill_id().to_string();

    ensure_debug_runtime_layout(&runtime_root)?;
    let synced_skill_path =
        synchronize_skill_into_runtime_root(&runtime_root, &source_skill_path, &skill_id)?;
    manifest.bind_directory_skill_id(skill_id.clone());

    Ok(DebugSkillSyncResult {
        skill_id,
        runtime_root,
        source_skill_path,
        synced_skill_path,
    })
}

/// Build the public sync output payload from the internal sync result.
/// 根据内部同步结果构建公开 sync 输出载荷。
///
/// The sync_result parameter keeps exact PathBuf values for internal execution flow.
/// sync_result 参数保留内部执行流使用的精确 PathBuf 值。
///
/// Return the serialized sync output with host-visible path strings.
/// 返回使用宿主可见路径字符串的序列化 sync 输出。
fn build_sync_output(sync_result: &DebugSkillSyncResult) -> DebugSyncOutput {
    DebugSyncOutput {
        command: "sync",
        skill_id: sync_result.skill_id.clone(),
        runtime_root: render_host_visible_path(&sync_result.runtime_root),
        source_skill_path: render_host_visible_path(&sync_result.source_skill_path),
        synced_skill_path: render_host_visible_path(&sync_result.synced_skill_path),
    }
}

/// Resolve the debug command target skill and optionally synchronize a source path first.
/// 解析调试命令的目标 skill，并在提供源路径时先执行同步。
fn resolve_debug_target(command: &DebugCliCommand) -> Result<(String, Option<PathBuf>), String> {
    if command.skill_path.is_some() {
        let sync_result = sync_debug_skill(command)?;
        return Ok((sync_result.skill_id, Some(sync_result.source_skill_path)));
    }
    let skill_id = command
        .skill_id
        .clone()
        .ok_or_else(|| "debug run requires --skill-id or --skill-path".to_string())?;
    Ok((skill_id, None))
}

/// Prepare the debug runtime by loading a previously synchronized skill through the normal engine path.
/// 通过正式引擎路径加载一个已经同步好的 skill，准备调试运行时。
fn prepare_debug_runtime(command: &DebugCliCommand) -> Result<PreparedDebugRuntime, String> {
    let runtime_root = absolutize_path(&command.runtime_root)?;
    ensure_debug_runtime_layout(&runtime_root)?;
    let (skill_id, source_skill_path) = resolve_debug_target(command)?;
    let synced_skill_path = runtime_root.join("skills").join(&skill_id);
    // Manifest path that proves the synchronized skill directory is ready to load.
    // 用于证明已同步 skill 目录可加载的清单路径。
    let synced_skill_manifest_path = synced_skill_path.join("skill.yaml");
    if !debug_synced_skill_manifest_path_is_file(&synced_skill_manifest_path)? {
        return Err(format!(
            "Synchronized skill '{}' was not found under '{}'. Run 'luaskills-debug sync --runtime-root {} --skill-path <source-skill>' first.",
            skill_id,
            render_debug_path(&synced_skill_path),
            render_debug_path(&runtime_root)
        ));
    }
    let manifest = load_bound_skill_manifest(&synced_skill_path)?;

    let ignored_skill_ids = collect_ignored_skill_ids(&runtime_root.join("skills"), &skill_id)?;
    let host_options = build_debug_host_options(&runtime_root, ignored_skill_ids);
    let pool_config = LuaVmPoolConfig {
        min_size: 1,
        max_size: 2,
        idle_ttl_secs: 30,
    };
    let mut engine = LuaEngine::new(LuaEngineOptions::new(pool_config, host_options))
        .map_err(|error| error.to_string())?;
    let skill_roots = [RuntimeSkillRoot {
        name: "ROOT".to_string(),
        skills_dir: runtime_root.join("skills"),
    }];
    engine
        .load_from_roots(&skill_roots)
        .map_err(|error| error.to_string())?;

    // Loaded runtime entry descriptors produced by the same public listing path as host callers.
    // 通过与宿主调用方相同的公开列表路径生成的已加载运行时入口描述。
    let runtime_entries = engine.list_entries().map_err(|error| error.to_string())?;
    let entries = filter_skill_entries(&runtime_entries, &skill_id);
    if entries.is_empty() {
        return Err(format!(
            "Skill '{}' loaded without any callable entries",
            skill_id
        ));
    }

    Ok(PreparedDebugRuntime {
        engine,
        manifest,
        skill_id,
        runtime_root,
        source_skill_path,
        synced_skill_path,
        entries,
    })
}

/// Inspect whether one synchronized skill manifest path is a file without hiding filesystem probe errors.
/// 检查单个已同步 skill 清单路径是否为文件，同时不隐藏文件系统探测错误。
///
/// The manifest_path parameter is the concrete `runtime_root/skills/<skill-id>/skill.yaml` path.
/// manifest_path 参数是具体的 `runtime_root/skills/<skill-id>/skill.yaml` 路径。
///
/// Return true for an existing manifest file, false for a confirmed missing manifest, or an explicit probe/type error.
/// 已存在清单文件返回 true，确认缺失清单返回 false；探测或类型异常时返回显式错误。
fn debug_synced_skill_manifest_path_is_file(manifest_path: &Path) -> Result<bool, String> {
    match fs::metadata(manifest_path) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(format!(
            "Synchronized skill manifest is not a file: '{}'",
            render_debug_path(manifest_path)
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "Failed to inspect synchronized skill manifest '{}': {}",
            render_debug_path(manifest_path),
            error
        )),
    }
}

/// Convert one possibly relative path into an absolute developer-facing path.
/// 将单个可能为相对路径的输入转换为面向开发者的绝对路径。
fn absolutize_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(env::current_dir()
            .map_err(|error| format!("Failed to resolve current directory: {}", error))?
            .join(path))
    }
}

/// Render one path for luaskills-debug user-visible diagnostics.
/// 为 luaskills-debug 用户可见诊断渲染单个路径。
fn render_debug_path(path: &Path) -> String {
    render_host_visible_path(path)
}

/// Load and bind one skill manifest from the source skill directory.
/// 从源 skill 目录加载并绑定单个 skill 清单。
fn load_bound_skill_manifest(skill_path: &Path) -> Result<SkillMeta, String> {
    if !debug_skill_source_path_is_directory(skill_path)? {
        return Err(format!(
            "Skill path '{}' is not a directory",
            render_debug_path(skill_path)
        ));
    }

    let directory_name = skill_path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            format!(
                "Skill path '{}' must end with one UTF-8 directory name",
                render_debug_path(skill_path)
            )
        })?;
    validate_luaskills_identifier(directory_name, "skill directory name")?;

    let manifest_path = skill_path.join("skill.yaml");
    let manifest_text = fs::read_to_string(&manifest_path).map_err(|error| {
        format!(
            "Failed to read skill manifest '{}': {}",
            render_debug_path(&manifest_path),
            error
        )
    })?;
    let mut manifest: SkillMeta = serde_yaml::from_str(&manifest_text).map_err(|error| {
        format!(
            "Failed to parse skill manifest '{}': {}",
            render_debug_path(&manifest_path),
            error
        )
    })?;
    manifest.bind_directory_skill_id(directory_name.to_string());
    manifest.resolve_entry_input_schemas(skill_path)?;
    validate_luaskills_identifier(manifest.effective_skill_id(), "skill_id")?;
    Ok(manifest)
}

/// Inspect whether one debug skill source path is a directory without hiding filesystem probe errors.
/// 检查单个调试 skill 源路径是否为目录，同时不隐藏文件系统探测错误。
///
/// The skill_path parameter is the developer-supplied source skill path after debug path absolutization.
/// skill_path 参数是经过调试路径绝对化后的开发者输入源 skill 路径。
///
/// Return true for an existing directory, false for a confirmed missing or non-directory path, or an explicit probe error.
/// 已存在目录返回 true，确认缺失或非目录路径返回 false；探测异常时返回显式错误。
fn debug_skill_source_path_is_directory(skill_path: &Path) -> Result<bool, String> {
    match fs::metadata(skill_path) {
        Ok(metadata) => Ok(metadata.is_dir()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "Failed to inspect skill path '{}': {}",
            render_debug_path(skill_path),
            error
        )),
    }
}

/// Ensure the runtime root contains the core directory layout expected by one normal skill runtime.
/// 确保运行时根目录包含正常 skill 运行时所期望的核心目录布局。
fn ensure_debug_runtime_layout(runtime_root: &Path) -> Result<(), String> {
    let required_directories = [
        runtime_root.to_path_buf(),
        runtime_root.join("skills"),
        runtime_root.join("temp"),
        runtime_root.join("temp").join("downloads"),
        runtime_root.join("resources"),
        runtime_root.join("lua_packages"),
        runtime_root.join("libs"),
        runtime_root.join("bin"),
        runtime_root.join("dependencies"),
        runtime_root.join("state"),
        runtime_root.join("databases"),
        runtime_root.join("config"),
        runtime_root.join("system_lua_lib"),
    ];

    for directory in required_directories {
        fs::create_dir_all(&directory).map_err(|error| {
            format!(
                "Failed to create runtime directory '{}': {}",
                render_debug_path(&directory),
                error
            )
        })?;
    }
    Ok(())
}

/// Synchronize the source skill directory into the target runtime `skills/<skill-id>` directory.
/// 将源 skill 目录同步到目标运行时的 `skills/<skill-id>` 目录下。
fn synchronize_skill_into_runtime_root(
    runtime_root: &Path,
    source_skill_path: &Path,
    skill_id: &str,
) -> Result<PathBuf, String> {
    let target_skill_path = runtime_root.join("skills").join(skill_id);
    if paths_refer_to_same_directory(source_skill_path, &target_skill_path)? {
        return Ok(target_skill_path);
    }

    if debug_sync_target_path_is_directory(&target_skill_path)? {
        fs::remove_dir_all(&target_skill_path).map_err(|error| {
            format!(
                "Failed to remove previous synchronized skill '{}': {}",
                render_debug_path(&target_skill_path),
                error
            )
        })?;
    }
    copy_directory_recursive(source_skill_path, &target_skill_path)?;
    Ok(target_skill_path)
}

/// Inspect whether one synchronized skill target path is a directory without hiding filesystem probe errors.
/// 检查单个已同步 skill 目标路径是否为目录，同时不隐藏文件系统探测错误。
///
/// The target_skill_path parameter is the concrete `runtime_root/skills/<skill-id>` synchronization target.
/// target_skill_path 参数是具体的 `runtime_root/skills/<skill-id>` 同步目标路径。
///
/// Return true for an existing directory, false for a confirmed missing target, or an explicit probe/type error.
/// 已存在目录返回 true，确认缺失目标返回 false；探测或类型异常时返回显式错误。
fn debug_sync_target_path_is_directory(target_skill_path: &Path) -> Result<bool, String> {
    match fs::metadata(target_skill_path) {
        Ok(metadata) if metadata.is_dir() => Ok(true),
        Ok(_) => Err(format!(
            "Previous synchronized skill path is not a directory: '{}'",
            render_debug_path(target_skill_path)
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "Failed to inspect previous synchronized skill '{}': {}",
            render_debug_path(target_skill_path),
            error
        )),
    }
}

/// Return whether two directory paths resolve to the same physical location when both already exist.
/// 当两个目录路径都已存在时，返回它们是否解析到同一物理位置。
fn paths_refer_to_same_directory(left: &Path, right: &Path) -> Result<bool, String> {
    if !same_directory_candidate_path_is_directory(left, "source")?
        || !same_directory_candidate_path_is_directory(right, "target")?
    {
        return Ok(false);
    }

    let left_canonical = fs::canonicalize(left).map_err(|error| {
        format!(
            "Failed to canonicalize source path '{}': {}",
            render_debug_path(left),
            error
        )
    })?;
    let right_canonical = fs::canonicalize(right).map_err(|error| {
        format!(
            "Failed to canonicalize target path '{}': {}",
            render_debug_path(right),
            error
        )
    })?;
    Ok(left_canonical == right_canonical)
}

/// Inspect whether one same-directory comparison candidate is an existing directory.
/// 检查单个同目录比较候选路径是否为已存在目录。
///
/// The path parameter is the concrete source or target path considered by same-directory comparison.
/// path 参数是同目录比较正在考虑的具体源路径或目标路径。
///
/// The path_label parameter identifies the candidate side in diagnostics.
/// path_label 参数在诊断中标识候选路径所在的一侧。
///
/// Return true only for an existing directory, false for a missing or non-directory path, or an explicit probe error.
/// 仅已存在目录返回 true，缺失或非目录路径返回 false；探测异常时返回显式错误。
fn same_directory_candidate_path_is_directory(
    path: &Path,
    path_label: &str,
) -> Result<bool, String> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.is_dir()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "Failed to inspect {} path '{}' before same-directory comparison: {}",
            path_label,
            render_debug_path(path),
            error
        )),
    }
}

/// Recursively copy one directory tree while rejecting symbolic links for predictable debug behavior.
/// 递归复制单个目录树，并拒绝符号链接以保证调试行为可预测。
fn copy_directory_recursive(source: &Path, target: &Path) -> Result<(), String> {
    fs::create_dir_all(target).map_err(|error| {
        format!(
            "Failed to create synchronized skill directory '{}': {}",
            render_debug_path(target),
            error
        )
    })?;

    for entry in fs::read_dir(source).map_err(|error| {
        format!(
            "Failed to enumerate skill directory '{}': {}",
            render_debug_path(source),
            error
        )
    })? {
        let entry = entry.map_err(|error| {
            format!(
                "Failed to read one directory entry under '{}': {}",
                render_debug_path(source),
                error
            )
        })?;
        let file_type = entry.file_type().map_err(|error| {
            format!(
                "Failed to inspect entry '{}' type: {}",
                render_debug_path(&entry.path()),
                error
            )
        })?;
        let destination = target.join(entry.file_name());
        if file_type.is_symlink() {
            return Err(format!(
                "Symbolic-link entry '{}' is not supported by luaskills-debug",
                render_debug_path(&entry.path())
            ));
        }
        if file_type.is_dir() {
            copy_directory_recursive(&entry.path(), &destination)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), &destination).map_err(|error| {
                format!(
                    "Failed to copy '{}' to '{}': {}",
                    render_debug_path(&entry.path()),
                    render_debug_path(&destination),
                    error
                )
            })?;
        }
    }
    Ok(())
}

/// Collect all other valid skill identifiers under the runtime `skills/` directory so they can be ignored.
/// 收集运行时 `skills/` 目录下除目标外的其他有效 skill 标识符，以便调试时忽略它们。
fn collect_ignored_skill_ids(
    skills_dir: &Path,
    target_skill_id: &str,
) -> Result<Vec<String>, String> {
    if !debug_runtime_skills_path_is_directory(skills_dir)? {
        return Ok(Vec::new());
    }

    let mut ignored = Vec::new();
    for entry in fs::read_dir(skills_dir).map_err(|error| {
        format!(
            "Failed to enumerate runtime skills directory '{}': {}",
            render_debug_path(skills_dir),
            error
        )
    })? {
        let entry = entry.map_err(|error| {
            format!(
                "Failed to read one runtime skill directory entry under '{}': {}",
                render_debug_path(skills_dir),
                error
            )
        })?;
        if !entry
            .file_type()
            .map_err(|error| {
                format!(
                    "Failed to inspect '{}': {}",
                    render_debug_path(&entry.path()),
                    error
                )
            })?
            .is_dir()
        {
            continue;
        }
        let Some(candidate_id) = entry.file_name().to_str().map(|value| value.to_string()) else {
            continue;
        };
        if candidate_id == target_skill_id {
            continue;
        }
        if validate_luaskills_identifier(&candidate_id, "skill_id").is_ok() {
            ignored.push(candidate_id);
        }
    }
    ignored.sort();
    Ok(ignored)
}

/// Inspect whether the runtime `skills/` path is a directory before collecting ignored skills.
/// 在收集忽略 skill 前检查运行时 `skills/` 路径是否为目录。
///
/// The skills_dir parameter is the concrete runtime `skills/` directory that will be enumerated.
/// skills_dir 参数是即将被枚举的具体运行时 `skills/` 目录。
///
/// Return true for an existing directory, false for a confirmed missing path, or an explicit probe/type error.
/// 已存在目录返回 true，确认缺失路径返回 false；探测或类型异常时返回显式错误。
fn debug_runtime_skills_path_is_directory(skills_dir: &Path) -> Result<bool, String> {
    match fs::metadata(skills_dir) {
        Ok(metadata) if metadata.is_dir() => Ok(true),
        Ok(_) => Err(format!(
            "Runtime skills path is not a directory: '{}'",
            render_debug_path(skills_dir)
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "Failed to inspect runtime skills directory '{}': {}",
            render_debug_path(skills_dir),
            error
        )),
    }
}

/// Build host options that map one debug runtime root into the normal LuaSkills runtime layout.
/// 构建宿主选项，将单个调试 runtime_root 映射为正常 LuaSkills 运行时布局。
fn build_debug_host_options(
    runtime_root: &Path,
    ignored_skill_ids: Vec<String>,
) -> LuaRuntimeHostOptions {
    let mut host_options = LuaRuntimeHostOptions::with_runtime_root(runtime_root.to_path_buf());
    host_options.allow_network_download = true;
    host_options.ignored_skill_ids = ignored_skill_ids;
    host_options
}

/// Filter all loaded runtime entries down to one exact skill identifier.
/// 将所有已加载运行时入口过滤到单个精确 skill 标识符范围内。
fn filter_skill_entries(
    entries: &[RuntimeEntryDescriptor],
    skill_id: &str,
) -> Vec<RuntimeEntryDescriptor> {
    let mut filtered = entries
        .iter()
        .filter(|entry| entry.skill_id == skill_id)
        .cloned()
        .collect::<Vec<_>>();
    filtered.sort_by(|left, right| left.canonical_name.cmp(&right.canonical_name));
    filtered
}

/// Resolve one developer-supplied tool name into the actual canonical runtime entry name.
/// 将开发者输入的工具名称解析为实际 canonical 运行时入口名称。
fn resolve_debug_tool_name(
    entries: &[RuntimeEntryDescriptor],
    requested_tool_name: &str,
) -> Result<String, String> {
    let normalized = requested_tool_name.trim();
    if normalized.is_empty() {
        return Err("Tool name must not be empty".to_string());
    }

    if let Some(entry) = entries
        .iter()
        .find(|entry| entry.canonical_name == normalized)
    {
        return Ok(entry.canonical_name.clone());
    }

    let local_matches = entries
        .iter()
        .filter(|entry| entry.local_name == normalized)
        .collect::<Vec<_>>();
    match local_matches.as_slice() {
        [entry] => Ok(entry.canonical_name.clone()),
        [] => Err(format!(
            "Tool '{}' not found. Available tools: {}",
            normalized,
            entries
                .iter()
                .map(|entry| entry.canonical_name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
        _ => Err(format!(
            "Tool '{}' is ambiguous within the loaded skill",
            normalized
        )),
    }
}

/// Load invocation args from CLI inline JSON or JSON file content.
/// 从命令行内联 JSON 或 JSON 文件内容中加载调用参数。
fn load_invocation_args(command: &DebugCliCommand) -> Result<Value, String> {
    match (&command.args_json, &command.args_file) {
        (Some(args_json), None) => serde_json::from_str(args_json)
            .map_err(|error| format!("Failed to parse --args-json: {}", error)),
        (None, Some(args_file)) => {
            let args_text = fs::read_to_string(args_file).map_err(|error| {
                format!(
                    "Failed to read args file '{}': {}",
                    render_debug_path(args_file),
                    error
                )
            })?;
            serde_json::from_str(&args_text).map_err(|error| {
                format!(
                    "Failed to parse args file '{}': {}",
                    render_debug_path(args_file),
                    error
                )
            })
        }
        (None, None) => Ok(json!({})),
        (Some(_), Some(_)) => Err("--args-json and --args-file are mutually exclusive".to_string()),
    }
}

/// Build the invocation context used by the debug call command.
/// 构建调试调用命令使用的调用上下文。
fn build_debug_invocation_context(enable_host_result: bool) -> LuaInvocationContext {
    if !enable_host_result {
        return LuaInvocationContext::empty();
    }

    let request_context = RuntimeRequestContext {
        client_capabilities: json!({
            "host_result": {
                "enabled": true
            }
        }),
        ..RuntimeRequestContext::default()
    };
    LuaInvocationContext::new(Some(request_context), json!({}), json!({}))
}

/// Build one structured inspect output payload from the prepared runtime state.
/// 基于已准备好的运行时状态构建单份结构化 inspect 输出载荷。
fn build_inspect_output(prepared: &PreparedDebugRuntime) -> DebugInspectOutput {
    DebugInspectOutput {
        command: "inspect",
        skill_id: prepared.skill_id.clone(),
        manifest_name: prepared.manifest.name.clone(),
        manifest_version: prepared.manifest.version().to_string(),
        debug: prepared.manifest.debug,
        runtime_root: render_host_visible_path(&prepared.runtime_root),
        source_skill_path: prepared
            .source_skill_path
            .as_ref()
            .map(|path| render_host_visible_path(path)),
        synced_skill_path: render_host_visible_path(&prepared.synced_skill_path),
        entries: prepared.entries.clone(),
    }
}

/// Render the sync command output in the requested mode.
/// 按指定模式渲染 sync 命令输出。
fn render_sync_output(mode: DebugOutputMode, output: &DebugSyncOutput) -> Result<(), String> {
    match mode {
        DebugOutputMode::Pretty => {
            println!("skill_id: {}", output.skill_id);
            println!("runtime_root: {}", output.runtime_root);
            println!("source_skill_path: {}", output.source_skill_path);
            println!("synced_skill_path: {}", output.synced_skill_path);
            Ok(())
        }
        DebugOutputMode::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(output)
                    .map_err(|error| format!("Failed to serialize sync output: {}", error))?
            );
            Ok(())
        }
        DebugOutputMode::Content => Err("sync does not support --output content".to_string()),
    }
}

/// Render the inspect command output in the requested mode.
/// 按指定模式渲染 inspect 命令输出。
fn render_inspect_output(mode: DebugOutputMode, output: &DebugInspectOutput) -> Result<(), String> {
    match mode {
        DebugOutputMode::Pretty => {
            println!("skill_id: {}", output.skill_id);
            println!("manifest_name: {}", output.manifest_name);
            println!("manifest_version: {}", output.manifest_version);
            println!("debug: {}", output.debug);
            println!("runtime_root: {}", output.runtime_root);
            if let Some(source_skill_path) = &output.source_skill_path {
                println!("source_skill_path: {}", source_skill_path);
            }
            println!("synced_skill_path: {}", output.synced_skill_path);
            println!("entries:");
            for entry in &output.entries {
                println!("  - {} ({})", entry.canonical_name, entry.local_name);
            }
            Ok(())
        }
        DebugOutputMode::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(output)
                    .map_err(|error| format!("Failed to serialize inspect output: {}", error))?
            );
            Ok(())
        }
        DebugOutputMode::Content => Err("inspect does not support --output content".to_string()),
    }
}

/// Render the list-tools command output in the requested mode.
/// 按指定模式渲染 list-tools 命令输出。
fn render_list_tools_output(
    mode: DebugOutputMode,
    entries: &[RuntimeEntryDescriptor],
) -> Result<(), String> {
    match mode {
        DebugOutputMode::Pretty => {
            for entry in entries {
                println!("{}  ->  {}", entry.local_name, entry.canonical_name);
            }
            Ok(())
        }
        DebugOutputMode::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(entries)
                    .map_err(|error| format!("Failed to serialize tool list: {}", error))?
            );
            Ok(())
        }
        DebugOutputMode::Content => {
            for entry in entries {
                println!("{}", entry.canonical_name);
            }
            Ok(())
        }
    }
}

/// Render the call command output in the requested mode.
/// 按指定模式渲染 call 命令输出。
fn render_call_output(mode: DebugOutputMode, output: &DebugCallOutput) -> Result<(), String> {
    match mode {
        DebugOutputMode::Pretty => {
            println!("skill_id: {}", output.skill_id);
            println!("requested_tool_name: {}", output.requested_tool_name);
            println!("resolved_tool_name: {}", output.resolved_tool_name);
            println!("runtime_root: {}", output.runtime_root);
            println!("synced_skill_path: {}", output.synced_skill_path);
            println!("content:");
            println!("{}", output.result.content);
            println!("overflow_mode: {:?}", output.result.overflow_mode);
            println!("template_hint: {:?}", output.result.template_hint);
            println!("content_bytes: {}", output.result.content_bytes);
            println!("content_lines: {}", output.result.content_lines);
            if let Some(host_result) = &output.result.host_result {
                println!("host_result.kind: {}", host_result.kind);
                println!(
                    "host_result.payload:\n{}",
                    serde_json::to_string_pretty(&host_result.payload).map_err(|error| {
                        format!("Failed to serialize host_result payload: {}", error)
                    })?
                );
            }
            Ok(())
        }
        DebugOutputMode::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(output)
                    .map_err(|error| format!("Failed to serialize call output: {}", error))?
            );
            Ok(())
        }
        DebugOutputMode::Content => {
            println!("{}", output.result.content);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DebugCliCommand, DebugCommandKind, DebugOutputMode, collect_ignored_skill_ids,
        load_bound_skill_manifest, load_invocation_args, parse_debug_cli,
        paths_refer_to_same_directory, prepare_debug_runtime, render_debug_path,
        resolve_debug_tool_name, sync_debug_skill,
    };
    use luaskills::{LuaInvocationContext, RuntimeEntryDescriptor};
    use std::env;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Build one minimal runtime entry descriptor for parser and resolver tests.
    /// 为参数解析与工具解析测试构造一个最小运行时入口描述。
    fn make_entry(local_name: &str, canonical_name: &str) -> RuntimeEntryDescriptor {
        RuntimeEntryDescriptor {
            canonical_name: canonical_name.to_string(),
            skill_id: "demo-skill".to_string(),
            local_name: local_name.to_string(),
            root_name: "ROOT".to_string(),
            skill_dir: "D:/demo-skill".to_string(),
            description: String::new(),
            parameters: Vec::new(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        }
    }

    /// Verify the call command accepts inline JSON args and explicit output mode.
    /// 验证 call 命令接受内联 JSON 参数和显式输出模式。
    #[test]
    fn parse_debug_cli_accepts_call_json_args() {
        let args = vec![
            "call".to_string(),
            "--runtime-root".to_string(),
            "D:/runtime".to_string(),
            "--skill-path".to_string(),
            "D:/skills/demo-skill".to_string(),
            "--tool".to_string(),
            "ping".to_string(),
            "--args-json".to_string(),
            "{\"x\":1}".to_string(),
            "--enable-host-result".to_string(),
            "--output".to_string(),
            "json".to_string(),
        ];
        let command = parse_debug_cli(&args).expect("parse call command");
        assert_eq!(command.kind, DebugCommandKind::Call);
        assert_eq!(command.output_mode, DebugOutputMode::Json);
        assert_eq!(command.tool_name.as_deref(), Some("ping"));
        assert_eq!(
            command.skill_path.as_deref(),
            Some(Path::new("D:/skills/demo-skill"))
        );
        assert!(command.enable_host_result);
    }

    /// Verify the parser accepts run commands against a previously synchronized skill id.
    /// 验证解析器接受面向已同步 skill 标识符的运行命令。
    #[test]
    fn parse_debug_cli_accepts_run_with_skill_id() {
        let args = vec![
            "call".to_string(),
            "--runtime-root".to_string(),
            "D:/runtime".to_string(),
            "--skill-id".to_string(),
            "demo-skill".to_string(),
            "--tool".to_string(),
            "ping".to_string(),
        ];
        let command = parse_debug_cli(&args).expect("parse skill-id call command");
        assert_eq!(command.kind, DebugCommandKind::Call);
        assert_eq!(command.skill_id.as_deref(), Some("demo-skill"));
        assert!(command.skill_path.is_none());
    }

    /// Verify the parser rejects providing both JSON arg sources at the same time.
    /// 验证当同时提供两种 JSON 参数来源时解析器会拒绝该输入。
    #[test]
    fn parse_debug_cli_rejects_duplicate_arg_sources() {
        let args = vec![
            "call".to_string(),
            "--runtime-root".to_string(),
            "D:/runtime".to_string(),
            "--skill-path".to_string(),
            "D:/skills/demo-skill".to_string(),
            "--tool".to_string(),
            "ping".to_string(),
            "--args-json".to_string(),
            "{\"x\":1}".to_string(),
            "--args-file".to_string(),
            "args.json".to_string(),
        ];
        let error = parse_debug_cli(&args).expect_err("duplicate arg sources should fail");
        assert!(error.contains("mutually exclusive"));
    }

    /// Verify local tool names resolve to their canonical runtime entry names.
    /// 验证局部工具名称会被解析为 canonical 运行时入口名称。
    #[test]
    fn resolve_debug_tool_name_accepts_local_name() {
        let entries = vec![
            make_entry("ping", "demo-skill-ping"),
            make_entry("read", "demo-skill-read"),
        ];
        let resolved = resolve_debug_tool_name(&entries, "read").expect("resolve local name");
        assert_eq!(resolved, "demo-skill-read");
    }

    /// Verify canonical tool names pass through unchanged.
    /// 验证 canonical 工具名称会原样通过解析。
    #[test]
    fn resolve_debug_tool_name_accepts_canonical_name() {
        let entries = vec![make_entry("ping", "demo-skill-ping")];
        let resolved =
            resolve_debug_tool_name(&entries, "demo-skill-ping").expect("resolve canonical name");
        assert_eq!(resolved, "demo-skill-ping");
    }

    /// Build one unique temporary runtime root path for integration-style debug tests.
    /// 为集成风格调试测试构建单个唯一的临时运行时根目录路径。
    fn make_temp_runtime_root() -> PathBuf {
        let unique_suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        env::temp_dir().join(format!("luaskills-debug-test-{}", unique_suffix))
    }

    /// Remove one temporary directory tree when it exists.
    /// 当临时目录树存在时移除该目录树。
    fn remove_temp_directory(path: &Path) {
        if path.exists() {
            fs::remove_dir_all(path).expect("temporary directory should be removable");
        }
    }

    /// Verify missing runtime skills directories produce an empty ignored-skill list.
    /// 验证缺失的运行时 skills 目录会生成空的忽略 skill 列表。
    #[test]
    fn collect_ignored_skill_ids_accepts_missing_skills_dir() {
        // Temporary runtime root that intentionally does not contain a skills directory.
        // 有意不包含 skills 目录的临时运行时根目录。
        let runtime_root = make_temp_runtime_root();
        // Missing runtime skills directory passed to the ignored-skill collector.
        // 传给忽略 skill 收集器的缺失运行时 skills 目录。
        let skills_dir = runtime_root.join("skills");

        // Ignored skill ids returned for a confirmed-missing skills directory.
        // 对确认缺失的 skills 目录返回的忽略 skill 标识符列表。
        let ignored = collect_ignored_skill_ids(&skills_dir, "demo-skill")
            .expect("missing skills directory should be accepted");

        assert!(ignored.is_empty());
    }

    /// Verify runtime skills path type errors are reported before directory enumeration.
    /// 验证运行时 skills 路径类型错误会在目录枚举前报告。
    #[test]
    fn collect_ignored_skill_ids_rejects_file_skills_dir() {
        // Temporary runtime root that isolates the file-backed skills directory fixture.
        // 隔离文件型 skills 目录夹具的临时运行时根目录。
        let runtime_root = make_temp_runtime_root();
        fs::create_dir_all(&runtime_root).expect("runtime root should be created");
        // Runtime skills path intentionally occupied by a regular file.
        // 被普通文件有意占用的运行时 skills 路径。
        let skills_dir = runtime_root.join("skills");
        fs::write(&skills_dir, b"not-a-directory").expect("skills fixture file should be written");

        // Error returned before read_dir can turn the type problem into an enumeration failure.
        // 在 read_dir 把类型问题变成枚举失败前返回的错误。
        let error = collect_ignored_skill_ids(&skills_dir, "demo-skill")
            .expect_err("file-backed skills path should fail");
        // Expected diagnostic rendered through the debug path formatter.
        // 通过调试路径渲染器生成的期望诊断。
        let expected_error = format!(
            "Runtime skills path is not a directory: '{}'",
            render_debug_path(&skills_dir)
        );

        assert_eq!(error, expected_error);
        remove_temp_directory(&runtime_root);
    }

    /// Verify runtime skills path probe errors are not treated as missing directories.
    /// 验证运行时 skills 路径探测错误不会被当作缺失目录处理。
    #[test]
    fn collect_ignored_skill_ids_reports_skills_dir_probe_errors() {
        // Runtime skills path containing an embedded NUL rejected by metadata probing.
        // 包含内嵌 NUL、会被元数据探测拒绝的运行时 skills 路径。
        let skills_dir = PathBuf::from("invalid\0skills");

        // Error returned before the invalid path can behave like a missing skills directory.
        // 在非法路径表现得像缺失 skills 目录之前返回的错误。
        let error = collect_ignored_skill_ids(&skills_dir, "demo-skill")
            .expect_err("invalid skills path probe should fail");

        assert!(
            error.contains("Failed to inspect runtime skills directory"),
            "unexpected error: {}",
            error
        );
        assert!(error.contains("invalid"), "unexpected error: {}", error);
    }

    /// Verify same-directory comparison reports source path probe errors explicitly.
    /// 验证同目录比较会显式报告源路径探测错误。
    #[test]
    fn paths_refer_to_same_directory_reports_source_probe_errors() {
        // Source path containing an embedded NUL rejected by metadata probing.
        // 包含内嵌 NUL、会被元数据探测拒绝的源路径。
        let source_path = PathBuf::from("invalid\0source");
        // Existing target directory that would otherwise be safe to compare.
        // 原本可安全参与比较的已存在目标目录。
        let runtime_root = make_temp_runtime_root();
        fs::create_dir_all(&runtime_root).expect("target directory should be created");

        // Error returned before canonicalization can hide the source probe failure.
        // 在 canonicalize 掩盖源路径探测失败前返回的错误。
        let error = paths_refer_to_same_directory(&source_path, &runtime_root)
            .expect_err("invalid source path probe should fail");

        assert!(
            error.contains("Failed to inspect source path"),
            "unexpected error: {}",
            error
        );
        assert!(error.contains("invalid"), "unexpected error: {}", error);
        remove_temp_directory(&runtime_root);
    }

    /// Verify same-directory comparison reports target path probe errors explicitly.
    /// 验证同目录比较会显式报告目标路径探测错误。
    #[test]
    fn paths_refer_to_same_directory_reports_target_probe_errors() {
        // Existing source directory that passes same-directory candidate probing.
        // 能通过同目录候选探测的已存在源目录。
        let runtime_root = make_temp_runtime_root();
        let source_path = runtime_root.join("source");
        fs::create_dir_all(&source_path).expect("source directory should be created");
        // Target path containing an embedded NUL rejected by metadata probing.
        // 包含内嵌 NUL、会被元数据探测拒绝的目标路径。
        let target_path = PathBuf::from("invalid\0target");

        // Error returned before canonicalization can hide the target probe failure.
        // 在 canonicalize 掩盖目标路径探测失败前返回的错误。
        let error = paths_refer_to_same_directory(&source_path, &target_path)
            .expect_err("invalid target path probe should fail");

        assert!(
            error.contains("Failed to inspect target path"),
            "unexpected error: {}",
            error
        );
        assert!(error.contains("invalid"), "unexpected error: {}", error);
        remove_temp_directory(&runtime_root);
    }

    /// Verify debug skill-manifest parse errors render paths through the debug path formatter.
    /// 验证调试 skill 清单解析错误会通过调试路径渲染器输出路径。
    #[test]
    fn load_bound_skill_manifest_parse_error_uses_host_visible_path() {
        // Temporary root that isolates the invalid debug skill fixture.
        // 隔离非法调试 skill 夹具的临时根目录。
        let runtime_root = make_temp_runtime_root();
        // Skill directory whose name passes the real directory-name identifier validation.
        // 目录名能通过真实目录名标识符校验的 skill 目录。
        let skill_dir = runtime_root.join("demo-skill");
        fs::create_dir_all(&skill_dir).expect("skill directory should be created");
        // Manifest path consumed by the real debug manifest loader.
        // 真实调试清单加载器消费的清单路径。
        let manifest_path = skill_dir.join("skill.yaml");
        fs::write(&manifest_path, "entries: [").expect("invalid manifest should be written");
        // Error returned by the real debug manifest loader.
        // 真实调试清单加载器返回的错误。
        let error =
            load_bound_skill_manifest(&skill_dir).expect_err("invalid skill manifest should fail");
        // Expected diagnostic prefix rendered with the debug path formatter.
        // 使用调试路径渲染器生成的期望诊断前缀。
        let expected_prefix = format!(
            "Failed to parse skill manifest '{}':",
            render_debug_path(&manifest_path)
        );

        assert!(
            error.starts_with(&expected_prefix),
            "unexpected error: {}",
            error
        );
        remove_temp_directory(&runtime_root);
    }

    /// Verify debug skill source-path probe errors are not reported as ordinary non-directory paths.
    /// 验证调试 skill 源路径探测错误不会被报告为普通非目录路径。
    #[test]
    fn load_bound_skill_manifest_reports_skill_path_probe_errors() {
        // Skill path containing an embedded NUL that filesystem metadata cannot inspect.
        // 包含内嵌 NUL 且文件系统元数据无法探测的 skill 路径。
        let skill_path = PathBuf::from("invalid\0skill");
        // Error returned before the invalid path can behave like a missing non-directory path.
        // 在非法路径表现得像缺失的非目录路径之前返回的错误。
        let error = match load_bound_skill_manifest(&skill_path) {
            Ok(_) => panic!("invalid skill path probe should fail"),
            Err(error) => error,
        };

        assert!(
            error.starts_with("Failed to inspect skill path"),
            "unexpected error: {}",
            error
        );
        assert!(error.contains("invalid"), "unexpected error: {}", error);
    }

    /// Verify debug args-file parse errors render paths through the debug path formatter.
    /// 验证调试参数文件解析错误会通过调试路径渲染器输出路径。
    #[test]
    fn load_invocation_args_file_parse_error_uses_host_visible_path() {
        // Temporary root that isolates the invalid args file fixture.
        // 隔离非法参数文件夹具的临时根目录。
        let runtime_root = make_temp_runtime_root();
        fs::create_dir_all(&runtime_root).expect("runtime root should be created");
        // Args file path consumed by the real invocation args loader.
        // 真实调用参数加载器消费的参数文件路径。
        let args_file = runtime_root.join("args.json");
        fs::write(&args_file, "{not-json").expect("invalid args file should be written");
        // Debug command that selects args-file loading through the normal command payload.
        // 通过常规命令载荷选择参数文件加载的调试命令。
        let command = DebugCliCommand {
            kind: DebugCommandKind::Call,
            runtime_root: runtime_root.clone(),
            skill_path: None,
            skill_id: Some("demo-skill".to_string()),
            tool_name: Some("ping".to_string()),
            args_json: None,
            args_file: Some(args_file.clone()),
            enable_host_result: false,
            output_mode: DebugOutputMode::Pretty,
        };
        // Error returned by the real invocation args loader.
        // 真实调用参数加载器返回的错误。
        let error = load_invocation_args(&command).expect_err("invalid args JSON should fail");
        // Expected diagnostic prefix rendered with the debug path formatter.
        // 使用调试路径渲染器生成的期望诊断前缀。
        let expected_prefix = format!(
            "Failed to parse args file '{}':",
            render_debug_path(&args_file)
        );

        assert!(
            error.starts_with(&expected_prefix),
            "unexpected error: {}",
            error
        );
        remove_temp_directory(&runtime_root);
    }

    /// Verify debug sync rejects file-backed synchronized skill targets before directory removal.
    /// 验证调试同步会在目录删除前拒绝文件型已同步 skill 目标。
    #[test]
    fn sync_debug_skill_rejects_file_target_skill_path() {
        // Temporary runtime root that isolates the file-backed sync target fixture.
        // 隔离文件型同步目标夹具的临时运行时根目录。
        let runtime_root = make_temp_runtime_root();
        // Source skill fixture loaded by the real debug sync path.
        // 真实调试同步路径加载的源 skill 夹具。
        let skill_path = PathBuf::from(
            "examples/ffi/standard_runtime/runtime_root/skills/demo-standard-ffi-skill",
        );
        // Target skill path derived from runtime_root, skills directory, and the source directory skill id.
        // 根据 runtime_root、skills 目录与源目录 skill id 派生出的目标 skill 路径。
        let target_skill_path = runtime_root.join("skills").join("demo-standard-ffi-skill");
        // Target parent directory required before writing the file-backed target.
        // 写入文件型目标前需要存在的目标父目录。
        let target_parent = target_skill_path
            .parent()
            .expect("target skill path should have a parent");
        fs::create_dir_all(target_parent).expect("target parent should be created");
        fs::write(&target_skill_path, b"not a directory").expect("file target should be written");
        // Debug sync command consumed by the real synchronization entry point.
        // 真实同步入口消费的调试同步命令。
        let command = DebugCliCommand {
            kind: DebugCommandKind::Sync,
            runtime_root: runtime_root.clone(),
            skill_path: Some(skill_path),
            skill_id: None,
            tool_name: None,
            args_json: None,
            args_file: None,
            enable_host_result: false,
            output_mode: DebugOutputMode::Pretty,
        };

        // Error returned before remove_dir_all can see the file-backed target path.
        // 在 remove_dir_all 看到文件型目标路径之前返回的错误。
        let error = match sync_debug_skill(&command) {
            Ok(_) => panic!("file target skill path should fail"),
            Err(error) => error,
        };

        assert!(
            error.contains("Previous synchronized skill path is not a directory"),
            "unexpected error: {}",
            error
        );
        assert!(
            error.contains(&render_debug_path(&target_skill_path)),
            "unexpected error: {}",
            error
        );
        remove_temp_directory(&runtime_root);
    }

    /// Verify the debug runtime can synchronize one skill into runtime_root and call it through the normal engine path.
    /// 验证调试运行时能够把单个 skill 同步进 runtime_root，并通过正式引擎路径完成调用。
    #[test]
    fn prepare_debug_runtime_loads_and_calls_skill_from_runtime_root() {
        let runtime_root = make_temp_runtime_root();
        let skill_path = PathBuf::from(
            "examples/ffi/standard_runtime/runtime_root/skills/demo-standard-ffi-skill",
        );
        let command = DebugCliCommand {
            kind: DebugCommandKind::Call,
            runtime_root: runtime_root.clone(),
            skill_path: Some(skill_path),
            skill_id: None,
            tool_name: Some("ping".to_string()),
            args_json: Some(r#"{"note":"from-debug-bin"}"#.to_string()),
            args_file: None,
            enable_host_result: false,
            output_mode: DebugOutputMode::Pretty,
        };

        let prepared = prepare_debug_runtime(&command).expect("runtime should prepare");
        let resolved_name =
            resolve_debug_tool_name(&prepared.entries, "ping").expect("tool should resolve");
        let args = load_invocation_args(&command).expect("args should parse");
        let result = prepared
            .engine
            .call_skill(&resolved_name, &args, Some(&LuaInvocationContext::empty()))
            .expect("skill call should succeed");

        assert_eq!(prepared.skill_id, "demo-standard-ffi-skill");
        assert_eq!(resolved_name, "demo-standard-ffi-skill-ping");
        assert_eq!(result.content, "standard-ffi-demo:from-debug-bin");
        assert!(prepared.synced_skill_path.exists());

        remove_temp_directory(&runtime_root);
    }

    /// Verify prepared debug runtime rejects directory-backed synchronized manifests before reading YAML.
    /// 验证调试运行时准备会在读取 YAML 前拒绝目录型已同步清单。
    #[test]
    fn prepare_debug_runtime_rejects_directory_synced_skill_manifest() {
        // Temporary runtime root that isolates the directory manifest fixture.
        // 隔离目录型清单夹具的临时运行时根目录。
        let runtime_root = make_temp_runtime_root();
        // Synchronized skill directory selected by the skill-id debug command.
        // skill-id 调试命令选中的已同步 skill 目录。
        let synced_skill_path = runtime_root.join("skills").join("demo-skill");
        // Manifest path deliberately occupied by a directory.
        // 被有意创建为目录的清单路径。
        let manifest_path = synced_skill_path.join("skill.yaml");
        fs::create_dir_all(&manifest_path).expect("directory manifest should be created");
        // Debug command that loads the pre-synchronized skill by id.
        // 通过 id 加载已同步 skill 的调试命令。
        let command = DebugCliCommand {
            kind: DebugCommandKind::Call,
            runtime_root: runtime_root.clone(),
            skill_path: None,
            skill_id: Some("demo-skill".to_string()),
            tool_name: Some("ping".to_string()),
            args_json: None,
            args_file: None,
            enable_host_result: false,
            output_mode: DebugOutputMode::Pretty,
        };

        // Error returned before the directory manifest can reach the YAML reader.
        // 在目录型清单进入 YAML 读取器之前返回的错误。
        let error = match prepare_debug_runtime(&command) {
            Ok(_) => panic!("directory synchronized manifest should fail"),
            Err(error) => error,
        };

        assert!(
            error.contains("Synchronized skill manifest is not a file"),
            "unexpected error: {}",
            error
        );
        assert!(
            error.contains(&render_debug_path(&manifest_path)),
            "unexpected error: {}",
            error
        );
        remove_temp_directory(&runtime_root);
    }

    /// Verify a synchronized runtime can be called by skill id without rewriting the skill directory.
    /// 验证已同步的运行时可以通过 skill id 调用，并且不会重写 skill 目录。
    #[test]
    fn prepare_debug_runtime_can_run_pre_synced_skill_by_id() {
        let runtime_root = make_temp_runtime_root();
        let skill_path = PathBuf::from(
            "examples/ffi/standard_runtime/runtime_root/skills/demo-standard-ffi-skill",
        );
        let sync_command = DebugCliCommand {
            kind: DebugCommandKind::Sync,
            runtime_root: runtime_root.clone(),
            skill_path: Some(skill_path),
            skill_id: None,
            tool_name: None,
            args_json: None,
            args_file: None,
            enable_host_result: false,
            output_mode: DebugOutputMode::Pretty,
        };
        let sync_output = sync_debug_skill(&sync_command).expect("sync should succeed");

        let run_command = DebugCliCommand {
            kind: DebugCommandKind::Call,
            runtime_root: runtime_root.clone(),
            skill_path: None,
            skill_id: Some(sync_output.skill_id.clone()),
            tool_name: Some("ping".to_string()),
            args_json: Some(r#"{"note":"from-synced-runtime"}"#.to_string()),
            args_file: None,
            enable_host_result: false,
            output_mode: DebugOutputMode::Pretty,
        };
        let prepared =
            prepare_debug_runtime(&run_command).expect("pre-synced runtime should prepare");
        let resolved_name =
            resolve_debug_tool_name(&prepared.entries, "ping").expect("tool should resolve");
        let args = load_invocation_args(&run_command).expect("args should parse");
        let result = prepared
            .engine
            .call_skill(&resolved_name, &args, Some(&LuaInvocationContext::empty()))
            .expect("skill call should succeed");

        assert_eq!(prepared.skill_id, "demo-standard-ffi-skill");
        assert!(prepared.source_skill_path.is_none());
        assert_eq!(result.content, "standard-ffi-demo:from-synced-runtime");

        remove_temp_directory(&runtime_root);
    }
}
