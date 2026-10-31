use luaskills::{
    LuaEngine, LuaEngineOptions, LuaInvocationContext, LuaRuntimeHostOptions, LuaVmPoolConfig,
    RuntimeHostToolAction, RuntimeHostToolCallback, RuntimeHostToolRequest, RuntimeSkillRoot,
    set_host_tool_callback,
};
use serde_json::{Value, json};
use std::{env, path::PathBuf, sync::Arc};

/// RAII guard that clears the process-wide demo host-tool callback on drop.
/// 在析构时清理进程级 demo 宿主工具回调的 RAII 守卫。
struct DemoHostToolCallbackGuard;

impl Drop for DemoHostToolCallbackGuard {
    fn drop(&mut self) {
        // Clear the process-wide callback so later demo code starts from a neutral host state.
        // 清理进程级回调，确保后续 demo 代码从中性的宿主状态开始。
        set_host_tool_callback(None);
    }
}

/// Run the direct Rust host demo against the shared standard runtime fixture.
/// 针对共享标准运行期样例执行 Rust 直连宿主 demo。
/// Returns `Ok(())` when both the skill-call and host-tool bridge demos finish.
/// 当 skill 调用与宿主工具桥接 demo 都完成时返回 `Ok(())`。
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Runtime root used by both Rust and FFI demos.
    // Rust 与 FFI demo 共用的运行根目录。
    // Manifest dir is the demo crate root in both repository and packaged layouts.
    // Manifest dir 在仓库布局和发布包布局中都指向 demo crate 根目录。
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    // Runtime root first honors packaged run scripts, then falls back to local repository fixtures.
    // Runtime root 优先使用发布包运行脚本注入的路径，再回退到本地仓库样例目录。
    let runtime_root = env::var_os("LUASKILLS_RUNTIME_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let packaged_runtime_root = manifest_dir.join("runtime");
            if packaged_runtime_root.exists() {
                packaged_runtime_root
            } else {
                manifest_dir.join("../ffi/standard_runtime/runtime_root")
            }
        });

    // Host options explicitly inject every runtime path instead of letting the library infer host policy.
    // 宿主选项显式注入每个运行期路径，避免让库自行推导宿主策略。
    let mut host_options = LuaRuntimeHostOptions::default();
    host_options.temp_dir = Some(runtime_root.join("temp"));
    host_options.resources_dir = Some(runtime_root.join("resources"));
    host_options.lua_packages_dir = Some(runtime_root.join("lua_packages"));
    host_options.host_provided_lua_root = Some(runtime_root.join("lua_packages"));
    host_options.host_provided_ffi_root = Some(runtime_root.join("libs"));
    host_options.host_provided_tool_root = Some(runtime_root.join("bin").join("tools"));
    host_options.download_cache_root = Some(runtime_root.join("temp").join("downloads"));
    host_options.dependency_dir_name = "dependencies".to_string();
    host_options.state_dir_name = "state".to_string();
    host_options.database_dir_name = "databases".to_string();

    // HostToolGuard keeps the demo host-tool callback installed for this process scope only.
    // HostToolGuard 只在当前进程作用域内保持 demo 宿主工具回调已安装。
    let _host_tool_guard = install_demo_host_tool_callback();

    // Pool config keeps the demo small while still using the real engine path.
    // 池配置保持 demo 轻量，同时仍走真实引擎路径。
    let pool_config = LuaVmPoolConfig {
        min_size: 1,
        max_size: 2,
        idle_ttl_secs: 30,
    };

    // Engine owns the Lua VM pool and loaded skill registry.
    // Engine 持有 Lua 虚拟机池和已加载 skill 注册表。
    let mut engine = LuaEngine::new(LuaEngineOptions::new(pool_config, host_options))?;

    // Skill roots tell the runtime which directory contains loadable skills.
    // Skill roots 告诉运行时哪个目录包含可加载的技能。
    let skill_roots = [RuntimeSkillRoot {
        // RootName uses the formal system layer required by the current runtime loader.
        // RootName 使用当前运行时加载器要求的正式系统层。
        name: "ROOT".to_string(),
        skills_dir: runtime_root.join("skills"),
    }];

    engine.load_from_roots(&skill_roots)?;

    run_skill_ping_demo(&engine)?;
    run_host_tool_bridge_demo(&engine)?;
    Ok(())
}

/// Run the existing skill-call demo path against the shared fixture skill.
/// 针对共享夹具 skill 执行既有的 skill 调用 demo 路径。
/// Parameter `engine` is the initialized LuaSkills runtime engine.
/// 参数 `engine` 是已初始化的 LuaSkills 运行时引擎。
/// Returns `Ok(())` after the fixture skill result has been printed.
/// 打印夹具 skill 结果后返回 `Ok(())`。
fn run_skill_ping_demo(engine: &LuaEngine) -> Result<(), Box<dyn std::error::Error>> {
    // InvocationContext is intentionally empty so the demo focuses on loading and calling.
    // InvocationContext 刻意保持为空，使 demo 聚焦加载与调用流程。
    let invocation_context = LuaInvocationContext::empty();

    // Result contains the text content returned by the fixture skill.
    // Result 包含夹具 skill 返回的文本内容。
    let result = engine.call_skill(
        "demo-standard-ffi-skill-ping",
        &json!({ "note": "rust" }),
        Some(&invocation_context),
    )?;

    println!("skill call demo:");
    println!("{}", result.content);
    Ok(())
}

/// Run a Lua snippet that exercises `vulcan.host.list / has / has_tool / call`.
/// 执行一段覆盖 `vulcan.host.list / has / has_tool / call` 的 Lua 片段。
/// Parameter `engine` is the initialized LuaSkills runtime engine.
/// 参数 `engine` 是已初始化的 LuaSkills 运行时引擎。
/// Returns `Ok(())` after the host-tool bridge JSON result has been printed.
/// 打印宿主工具桥接 JSON 结果后返回 `Ok(())`。
fn run_host_tool_bridge_demo(engine: &LuaEngine) -> Result<(), Box<dyn std::error::Error>> {
    // HostBridgeResult is a structured JSON value converted from the Lua return table.
    // HostBridgeResult 是从 Lua 返回 table 转换出的结构化 JSON 值。
    let host_bridge_result = engine.run_lua(
        r#"
local tools = vulcan.host.list()
local called = vulcan.host.call("model.embed", {
  model = "mock-embedding",
  input = args.input,
  stream = false,
  thinking = false,
})

return {
  first_tool = tools[1] and tools[1].name or nil,
  tool_count = #tools,
  has_embed = vulcan.host.has("model.embed"),
  has_embed_alias = vulcan.host.has_tool("model.embed"),
  has_missing = vulcan.host.has("missing.tool"),
  called_ok = called.ok,
  model = called.value.model,
  input = called.value.input,
  embedding = called.value.embedding,
  stream = called.meta.stream,
  thinking = called.meta.thinking,
}
"#,
        &json!({ "input": "hello from rust host" }),
        None,
    )?;

    println!("host-tool bridge demo:");
    println!("{}", serde_json::to_string_pretty(&host_bridge_result)?);
    Ok(())
}

/// Install the mock host-tool callback used by the Rust demo.
/// 安装 Rust demo 使用的 mock 宿主工具回调。
/// Returns a guard that clears the callback when it leaves scope.
/// 返回一个离开作用域时会清理回调的守卫。
fn install_demo_host_tool_callback() -> DemoHostToolCallbackGuard {
    // Callback delegates every Lua-triggered host-tool request into the local demo handler.
    // Callback 将每个由 Lua 触发的宿主工具请求转交给本地 demo 处理器。
    let callback: RuntimeHostToolCallback =
        Arc::new(|request: &RuntimeHostToolRequest| handle_demo_host_tool_request(request));

    set_host_tool_callback(Some(callback));
    DemoHostToolCallbackGuard
}

/// Handle one structured host-tool request from Lua.
/// 处理一个来自 Lua 的结构化宿主工具请求。
/// Parameter `request` contains the action, optional tool name, and JSON arguments.
/// 参数 `request` 包含动作、可选工具名和 JSON 参数。
/// Returns the host-visible JSON response or a diagnostic error string.
/// 返回宿主可见的 JSON 响应或诊断错误字符串。
fn handle_demo_host_tool_request(request: &RuntimeHostToolRequest) -> Result<Value, String> {
    match &request.action {
        RuntimeHostToolAction::List => Ok(json!([
            {
                "name": "model.embed",
                "description": "Mock embedding model exposed by the Rust demo host",
                "input_schema": {
                    "type": "object",
                    "required": ["input"],
                    "properties": {
                        "model": { "type": "string" },
                        "input": { "type": "string" },
                        "stream": { "type": "boolean" },
                        "thinking": { "type": "boolean" }
                    }
                }
            }
        ])),
        RuntimeHostToolAction::Has => {
            // ToolNameExists keeps the Lua-facing `has` path aligned with the call allowlist.
            // ToolNameExists 让 Lua 侧 `has` 路径与调用白名单保持一致。
            let tool_name_exists = request
                .tool_name
                .as_deref()
                .is_some_and(is_demo_host_tool_name);

            Ok(json!(tool_name_exists))
        }
        RuntimeHostToolAction::Call => call_demo_host_tool(request),
    }
}

/// Return whether one tool name is exposed by this demo host.
/// 返回某个工具名是否由当前 demo 宿主开放。
/// Parameter `tool_name` is the Lua-requested host tool name.
/// 参数 `tool_name` 是 Lua 请求的宿主工具名。
/// Returns `true` only for the demo `model.embed` tool.
/// 仅当工具为 demo 的 `model.embed` 时返回 `true`。
fn is_demo_host_tool_name(tool_name: &str) -> bool {
    tool_name == "model.embed"
}

/// Execute one mock host tool call and return the stable result envelope.
/// 执行一次 mock 宿主工具调用并返回稳定结果包络。
/// Parameter `request` contains the host tool name and JSON call arguments.
/// 参数 `request` 包含宿主工具名和 JSON 调用参数。
/// Returns a Lua-facing `{ ok, value | error, meta }` JSON envelope.
/// 返回面向 Lua 的 `{ ok, value | error, meta }` JSON 包络。
fn call_demo_host_tool(request: &RuntimeHostToolRequest) -> Result<Value, String> {
    // ToolName is required for the call path and is checked against the demo allowlist.
    // ToolName 是调用路径必需字段，并会按 demo 白名单校验。
    let tool_name = request.tool_name.as_deref().unwrap_or_default();

    if !is_demo_host_tool_name(tool_name) {
        return Ok(json!({
            "ok": false,
            "error": {
                "code": "host_tool_not_found",
                "message": format!("host tool not found: {}", tool_name),
            }
        }));
    }

    // Model is a caller-selectable label only; the demo never opens a network model provider.
    // Model 只是调用方可选的标签；该 demo 不会打开网络模型 provider。
    let model = request
        .args
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("mock-embedding");

    // Input is the text that the mock embedding function deterministically converts into floats.
    // Input 是 mock embedding 函数会确定性转换为浮点数组的文本。
    let input = request
        .args
        .get("input")
        .and_then(Value::as_str)
        .unwrap_or_default();

    Ok(json!({
        "ok": true,
        "value": {
            "model": model,
            "input": input,
            "embedding": demo_embedding_for(input),
        },
        "meta": {
            "tool": tool_name,
            "stream": false,
            "thinking": false,
        }
    }))
}

/// Produce a tiny deterministic embedding vector for demo output only.
/// 仅为 demo 输出生成一个小型确定性 embedding 向量。
/// Parameter `input` is the text to fold into four numeric buckets.
/// 参数 `input` 是要折叠进四个数值桶的文本。
/// Returns a four-dimensional mock embedding vector.
/// 返回四维 mock embedding 向量。
fn demo_embedding_for(input: &str) -> Vec<f64> {
    // Buckets keep the demo deterministic without pretending to be a real embedding model.
    // Buckets 让 demo 保持确定性，同时不伪装成真实 embedding 模型。
    let mut buckets = [0_u32; 4];

    for (index, byte) in input.bytes().enumerate() {
        // BucketIndex distributes bytes across four dimensions for compact, inspectable output.
        // BucketIndex 将字节分布到四个维度，便于输出保持紧凑且可检查。
        let bucket_index = index % buckets.len();
        buckets[bucket_index] = buckets[bucket_index].saturating_add(byte as u32);
    }

    // Denominator normalizes the vector for empty and non-empty inputs with the same code path.
    // Denominator 让空输入与非空输入共用同一条归一化路径。
    let denominator = input.len().max(1) as f64;

    buckets
        .iter()
        .map(|value| {
            // RoundedValue keeps the console output stable and readable across platforms.
            // RoundedValue 让控制台输出在不同平台上保持稳定且易读。
            let rounded_value = ((*value as f64 / denominator) / 255.0 * 1000.0).round() / 1000.0;
            rounded_value
        })
        .collect()
}
