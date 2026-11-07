//! runLua and runtime-lease timeouts that protected calls, the JIT and finalizers cannot get around.
//! 保护调用、JIT 与终结器都无法绕开的 runLua 和运行时租约超时。

use super::*;

/// Retry loops that catch the timeout with a protected call each one, keyed by a short label.
/// 每次都用保护调用捕获超时的重试循环，以简短标签为键。
const PROTECTED_SPINS: [(&str, &str); 4] = [
    (
        "pcall",
        "local function spin() while true do end end\nwhile true do pcall(spin) end",
    ),
    (
        "xpcall",
        "local function spin() while true do end end\nwhile true do xpcall(spin, function(e) return e end) end",
    ),
    (
        "coroutine",
        "local function spin() while true do end end\nwhile true do coroutine.resume(coroutine.create(spin)) end",
    ),
    (
        "jit on",
        "if jit.on then jit.on() end\nlocal ok, j = pcall(require, 'jit')\nif ok and j.on then j.on() end\nlocal x = 0\nwhile true do x = x + 1 end",
    ),
];

/// Render one runLua request with a 200 ms timeout.
/// 以 200 毫秒超时渲染一次 runLua 请求。
fn runlua(engine: &LuaEngine, code: &str) -> String {
    let request = json!({ "code": code, "timeout_ms": 200 });
    match engine.execute_runlua_request_json_inline(&request.to_string()) {
        Ok(text) | Err(text) => text,
    }
}

/// Evaluate `code` in an existing runtime lease with a 200 ms timeout.
/// 在现有运行时租约中以 200 毫秒超时执行 `code`。
fn lease_eval(engine: &LuaEngine, lease_id: &Value, code: &str) -> String {
    let request = json!({ "lease_id": lease_id, "code": code, "timeout_ms": 200 });
    match engine.eval_runtime_lease_json(&request.to_string()) {
        Ok(text) | Err(text) => text,
    }
}

/// A runLua timeout ends the evaluation even when the code catches it and keeps retrying.
/// 即使代码捕获超时并不断重试，runLua 超时仍会结束这次执行。
#[test]
fn runlua_timeout_cannot_be_caught_or_outrun() {
    for (label, code) in PROTECTED_SPINS {
        let result = run_engine_operation_with_watchdog(move |engine| runlua(engine, code));
        assert!(result.contains("timed out"), "{label}: {result}");
    }
}

/// A runtime-lease timeout ends the evaluation, and the next evaluation gets a fresh budget.
/// 运行时租约超时会结束这次执行，下一次执行获得新的预算。
#[test]
fn runtime_lease_timeout_cannot_be_caught_and_resets_for_next_eval() {
    for (label, code) in PROTECTED_SPINS {
        let (spun, next) = run_engine_operation_with_watchdog(move |engine| {
            let created: Value = serde_json::from_str(
                &engine
                    .create_runtime_lease_json(r#"{"sid":"protected-spin","ttl_sec":60}"#)
                    .expect("create runtime lease"),
            )
            .expect("create response json");
            let spun = lease_eval(engine, &created["lease_id"], code);
            let next = lease_eval(
                engine,
                &created["lease_id"],
                "local ok, message = pcall(error, 'caught-normally')\nreturn tostring(ok) .. ':' .. tostring(message)",
            );
            format!("{spun}\u{1}{next}")
        })
        .split_once('\u{1}')
        .map(|(spun, next)| (spun.to_owned(), next.to_owned()))
        .unwrap();
        assert!(spun.contains("timed out"), "{label}: {spun}");
        assert!(next.contains("false:caught-normally"), "{label}: {next}");
    }
}

/// Lua code keeps the platform fields of `jit` but cannot turn the compiler on, install hooks, or
/// register finalizers; ordinary metatables and introspection keep working.
/// Lua 代码保留 `jit` 的平台字段，但不能开启编译器、安装钩子或注册终结器；普通元表与内省照常可用。
#[test]
fn runlua_restricts_jit_hooks_and_finalizers() {
    let engine = make_runtime_test_engine();
    let code = r#"
local loaded_ok, loaded = pcall(require, 'jit')
local opt_ok = pcall(require, 'jit.opt')
local gc_ok, gc_error = pcall(setmetatable, {}, { __gc = function() end })
local plain = setmetatable({}, { __index = function() return 'plain' end })
local probe = {
  os = type(jit.os), arch = type(jit.arch), on = type(jit.on), status = jit.status(),
  require_same = loaded_ok and loaded == jit, opt = opt_ok,
  newproxy = type(newproxy), sethook = type(debug.sethook),
  debug_setmetatable = type(debug.setmetatable), getupvalue = type(debug.getupvalue),
  getmetatable = type(debug.getmetatable),
  gc = gc_ok, gc_error = tostring(gc_error):find('__gc metamethods are not supported', 1, true) ~= nil,
  plain = plain.anything,
}
local keys = {}
for key in pairs(probe) do keys[#keys + 1] = key end
table.sort(keys)
local parts = {}
for _, key in ipairs(keys) do parts[#parts + 1] = key .. '=' .. tostring(probe[key]) end
return table.concat(parts, ',')
"#;
    let result = runlua(&engine, code);
    assert!(
        result.contains(
            "arch=string,debug_setmetatable=nil,gc=false,gc_error=true,getmetatable=function,\
getupvalue=function,newproxy=nil,on=nil,opt=false,os=string,plain=plain,require_same=true,\
sethook=nil,status=false"
        ),
        "unexpected result: {result}"
    );
}

/// Skill entry that caches `pcall` and `setmetatable` while it loads, as many Lua libraries do.
/// 在加载时缓存 `pcall` 与 `setmetatable` 的技能入口，许多 Lua 库都这样写。
const CACHING_SKILL_SOURCE: &str = r#"local cached_pcall = pcall
local cached_setmetatable = setmetatable
local function spin() while true do end end
return function(args)
  if args.mode == "gc" then
    local ok = cached_pcall(cached_setmetatable, {}, { __gc = function() end })
    return tostring(ok)
  end
  while true do cached_pcall(spin) end
end
"#;

/// Builtins a skill caches while it loads are already the hardened ones, so a retry loop inside the
/// skill still ends at the runLua deadline and cannot register a finalizer.
/// 技能加载时缓存的内置函数已是加固后的版本，因此技能内部的重试循环仍会在 runLua 截止时间结束，
/// 也无法注册终结器。
#[test]
fn runlua_hardens_builtins_before_skill_code_loads() {
    let (result_tx, result_rx) = mpsc::channel();
    thread::spawn(move || {
        let runtime_root = make_temp_runtime_root("budget-cached-builtins");
        if runtime_root.exists() {
            let _ = fs::remove_dir_all(&runtime_root);
        }
        let root = RuntimeSkillRoot {
            name: "ROOT".to_string(),
            skills_dir: runtime_root.join("skills"),
        };
        write_model_test_skill_to_root(
            &root.skills_dir,
            "vulcan-cached-builtins",
            CACHING_SKILL_SOURCE,
        );
        let mut engine = make_runtime_test_engine();
        engine
            .load_from_roots(&[root])
            .expect("fixture skill should load");
        let spun = runlua(
            &engine,
            "return vulcan.call('vulcan-cached-builtins-ping', {})",
        );
        let gc = runlua(
            &engine,
            "return vulcan.call('vulcan-cached-builtins-ping', { mode = 'gc' })",
        );
        drop(engine);
        let _ = fs::remove_dir_all(&runtime_root);
        let _ = result_tx.send((spun, gc));
    });
    let (spun, gc) = result_rx
        .recv_timeout(Duration::from_secs(60))
        .expect("a skill's cached pcall must not outlive the runLua deadline");
    assert!(spun.contains("timed out"), "unexpected result: {spun}");
    assert!(gc.contains("false"), "unexpected result: {gc}");
}
