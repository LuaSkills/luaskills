//! Execution budgets that Lua code cannot catch, switch off, or outrun.
//! Lua 代码无法捕获、关闭或绕开的执行预算。
//!
//! A budget is one check installed as an instruction hook. Once the check fails it keeps failing,
//! and `pcall`, `xpcall` and `coroutine.resume` re-raise its error instead of swallowing it, so a
//! retry loop around protected calls still ends. The JIT compiler, whose traces skip hooks, stays
//! off for the whole life of the VM, and Lua code cannot install `__gc` finalizers, which LuaJIT
//! runs with hooks disabled.
//! 预算是作为指令钩子安装的一个检查。检查一旦失败就会一直失败，`pcall`、`xpcall` 与
//! `coroutine.resume` 会重新抛出该错误而不是吞掉它，因此包在保护调用外的重试循环也会结束。
//! 跟踪代码会跳过钩子的 JIT 编译器在 VM 整个生命周期内保持关闭；Lua 代码也不能安装 `__gc`
//! 终结器，因为 LuaJIT 运行终结器时会屏蔽钩子。

use crate::runtime::engine::LUA_BUDGET_HOOK_INTERVAL;
use crate::runtime::engine::vm_closing::HostFunctions;
use mlua::{HookTriggers, Lua, VmState, ffi};
use std::ffi::c_int;
use std::sync::Arc;

unsafe extern "C" {
    /// LuaJIT's mode switch from `luajit.h`; it returns 0 when the mode cannot be applied.
    /// `luajit.h` 中 LuaJIT 的模式开关；无法应用该模式时返回 0。
    fn luaJIT_setmode(state: *mut ffi::lua_State, idx: c_int, mode: c_int) -> c_int;
}

/// `LUAJIT_MODE_ENGINE`: the mode applies to the whole JIT compiler.
/// `LUAJIT_MODE_ENGINE`：模式作用于整个 JIT 编译器。
const LUAJIT_MODE_ENGINE: c_int = 0;
/// `LUAJIT_MODE_OFF`: switch the selected mode off.
/// `LUAJIT_MODE_OFF`：关闭所选模式。
const LUAJIT_MODE_OFF: c_int = 0x0000;
/// `LUAJIT_MODE_FLUSH`: discard every compiled trace.
/// `LUAJIT_MODE_FLUSH`：丢弃全部已编译跟踪。
const LUAJIT_MODE_FLUSH: c_int = 0x0200;

/// Budget check shared by the instruction hook and the protected-call wrappers.
/// 由指令钩子与保护调用包装共享的预算检查。
type BudgetCheck = Arc<dyn Fn() -> mlua::Result<()> + Send + Sync>;

/// Active budget of one VM, stored as app data while an evaluation runs.
/// 单个 VM 的当前预算，在一次执行期间作为应用数据保存。
struct ActiveBudget(BudgetCheck);

/// Install `check` as the budget of `lua` until [`remove`] is called.
/// 将 `check` 安装为 `lua` 的预算，直到调用 [`remove`]。
///
/// `check` must keep returning an error once it has returned one; the error text is what callers
/// observe when the budget runs out.
/// `check` 一旦返回错误就必须持续返回错误；预算用尽时调用方看到的就是该错误文本。
pub(crate) fn install(
    lua: &Lua,
    check: impl Fn() -> mlua::Result<()> + Send + Sync + 'static,
) -> mlua::Result<()> {
    let check: BudgetCheck = Arc::new(check);
    lua.set_app_data(ActiveBudget(Arc::clone(&check)));
    // LuaJIT keeps one hook for the whole state, so it also fires inside coroutines. mlua's
    // per-thread hook looks its callback up by the running coroutine and clears the hook when that
    // coroutine has none, so entering any coroutine would silently end the budget. The global hook
    // reads one callback for every thread.
    // LuaJIT 整个状态只有一个钩子，因此它在协程内同样会触发。mlua 的线程级钩子按正在运行的协程
    // 查找回调，找不到就清除钩子，于是进入任意协程都会悄无声息地终止预算。全局钩子对所有线程读取
    // 同一个回调。
    let installed = lua.set_global_hook(
        HookTriggers::new().every_nth_instruction(LUA_BUDGET_HOOK_INTERVAL),
        move |_, _| {
            check()?;
            Ok(VmState::Continue)
        },
    );
    if installed.is_err() {
        remove(lua);
    }
    installed
}

/// Remove the budget installed by [`install`]; protected calls then behave as plain Lua again.
/// 移除由 [`install`] 安装的预算；之后保护调用恢复为普通 Lua 行为。
pub(crate) fn remove(lua: &Lua) {
    // Dropping the callback alone leaves LuaJIT's hook armed until it next fires, so clear both.
    // 只丢弃回调会让 LuaJIT 的钩子保持启用直到下次触发，因此两者都要清除。
    lua.remove_global_hook();
    lua.remove_hook();
    lua.remove_app_data::<ActiveBudget>();
}

/// Run the active budget check of `lua`, if any.
/// 运行 `lua` 当前的预算检查（如果有）。
fn check_active(lua: &Lua) -> mlua::Result<()> {
    // Clone the check out so no app-data borrow is held while it runs.
    // 先克隆检查函数，使其运行期间不持有应用数据借用。
    let check = lua
        .app_data_ref::<ActiveBudget>()
        .map(|budget| Arc::clone(&budget.0));
    match check {
        Some(check) => check(),
        None => Ok(()),
    }
}

/// Lua prelude that closes every route around the budget.
/// 封堵所有绕开预算途径的 Lua 前置代码。
///
/// Its arguments are `budget_check` and whether to keep `newproxy` as `test_newproxy`, which only
/// test builds do so tests can still drive the host's VM-retirement paths with real finalizers.
/// 参数为 `budget_check`，以及是否把 `newproxy` 保留为 `test_newproxy`；只有测试构建会保留，
/// 以便测试仍能用真实终结器驱动宿主的 VM 退役路径。
///
/// Every library is optional, so the same prelude serves full VMs and the reduced validator state.
/// 每个库都是可选的，因此同一段前置代码同时适用于完整 VM 与精简的校验器状态。
const HARDEN_PRELUDE: &str = r#"
local budget_check, keep_test_newproxy = ...
local raw_setmetatable, raw_rawget, raw_type, raw_error = setmetatable, rawget, type, error

-- A failed protected call re-raises once the budget is spent, so retry loops cannot outlive it.
local function settle(ok, ...)
  if not ok then
    budget_check()
  end
  return ok, ...
end
local raw_pcall, raw_xpcall = pcall, xpcall
pcall = function(f, ...)
  return settle(raw_pcall(f, ...))
end
xpcall = function(f, handler, ...)
  return settle(raw_xpcall(f, handler, ...))
end
if raw_type(coroutine) == "table" then
  local raw_resume = coroutine.resume
  coroutine.resume = function(co, ...)
    return settle(raw_resume(co, ...))
  end
end

-- LuaJIT runs finalizers with hooks disabled, so Lua code may not register any.
if keep_test_newproxy then
  test_newproxy = newproxy
end
newproxy = nil
setmetatable = function(target, metatable)
  if raw_type(metatable) == "table" and raw_rawget(metatable, "__gc") ~= nil then
    raw_error("setmetatable: __gc metamethods are not supported", 2)
  end
  return raw_setmetatable(target, metatable)
end
if raw_type(debug) == "table" then
  debug.sethook = nil
  debug.setmetatable = nil
end

-- The host has already switched the compiler off; keep only the platform fields skills read, so
-- Lua code cannot switch it back on.
if raw_type(jit) == "table" then
  local raw_jit = jit
  local view = {
    os = raw_jit.os,
    arch = raw_jit.arch,
    version = raw_jit.version,
    version_num = raw_jit.version_num,
    status = function() return false end,
    off = function() end,
    flush = function() end,
  }
  jit = view
  if raw_type(package) == "table" then
    for _, name in ipairs({ "jit", "jit.opt", "jit.util", "jit.profile" }) do
      if raw_type(package.loaded) == "table" then package.loaded[name] = nil end
      if raw_type(package.preload) == "table" then package.preload[name] = nil end
    end
    package.loaded.jit = view
  end
end
"#;

/// Close every route around execution budgets in `lua`; call once, before any untrusted code runs.
/// 在 `lua` 中封堵所有绕开执行预算的途径；须在任何不受信任代码运行前调用一次。
///
/// Afterwards the JIT compiler is off and cannot be switched back on, `jit` keeps only its
/// platform fields, `newproxy`, `debug.sethook` and `debug.setmetatable` are gone, `setmetatable`
/// rejects `__gc`, and protected calls re-raise a spent budget.
/// 调用后 JIT 编译器关闭且无法重新开启，`jit` 只保留平台字段，`newproxy`、`debug.sethook`、
/// `debug.setmetatable` 被移除，`setmetatable` 拒绝 `__gc`，保护调用会重新抛出已用尽的预算。
pub(crate) fn harden(lua: &Lua) -> mlua::Result<()> {
    // Switch the compiler off through the C API, so a state opened without the `jit` library, such
    // as the config validator's, is covered as well.
    // 通过 C API 关闭编译器，使未打开 `jit` 库的状态（例如配置校验器的状态）同样受到覆盖。
    let switched_off = lua.exec_raw_lua(|raw| unsafe {
        luaJIT_setmode(raw.state(), 0, LUAJIT_MODE_ENGINE | LUAJIT_MODE_FLUSH) != 0
            && luaJIT_setmode(raw.state(), 0, LUAJIT_MODE_ENGINE | LUAJIT_MODE_OFF) != 0
    });
    if !switched_off {
        return Err(mlua::Error::runtime(
            "execution budget: failed to switch the LuaJIT compiler off",
        ));
    }
    let budget_check = lua.create_host_function(|lua, ()| check_active(lua))?;
    lua.load(HARDEN_PRELUDE)
        .set_name("=execution_budget")
        .call::<()>((budget_check, cfg!(test)))
}
