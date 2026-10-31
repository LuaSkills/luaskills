//! Own LuaJIT states allocated through its address-constrained native allocator.
//! 拥有通过 LuaJIT 地址受限原生分配器创建的状态。

use mlua::{Lua, StdLib, ffi};
use std::{ops::Deref, ptr::NonNull};

/// Mark a core-owned state before native close starts running remaining finalizers.
/// 在原生关闭开始运行剩余终结器之前标记核心拥有的状态。
pub(crate) struct Closing;

/// Exclusively own one native state and borrow mlua's registry-owned facade.
/// 独占一个原生状态，并借用 mlua 由注册表拥有的外观。
/// All Lua references and temporary guards stay inside this crate and borrow this owner.
/// 所有 Lua 引用及临时保护对象均留在本 crate 内并借用此所有者。
/// Public engine APIs exchange JSON or host capabilities, never these Lua handles.
/// 公开引擎接口交换 JSON 或宿主能力，绝不暴露这些 Lua 句柄。
pub(crate) struct LuaState {
    /// Native state remains valid until this owner's single lua_close call.
    /// 原生状态在此所有者唯一一次 lua_close 调用前保持有效。
    state: NonNull<ffi::lua_State>,
    /// Initialization may fail before the registry facade exists; the native state still needs closing.
    /// 初始化可能在注册表外观建立前失败，此时仍需关闭原生状态。
    facade: Option<NonNull<Lua>>,
}

// A state moves only with its exclusive owner; mlua serializes subsequent operations through its mutex.
// 状态仅随独占所有者移动；mlua 通过其互斥锁串行化后续操作。
unsafe impl Send for LuaState {}

impl LuaState {
    /// Create a native-allocated state with exactly libs; return its owner or an initialization error.
    /// 使用原生分配器创建仅含 libs 的状态；返回所有者或初始化错误。
    /// The caller selects permitted libraries; ALL intentionally permits native extensions for runtime skills.
    /// 调用者选择允许的库；运行时技能使用 ALL 时有意允许原生扩展。
    pub(crate) fn new(libs: StdLib) -> mlua::Result<Self> {
        // LuaJIT's allocator enforces its GC pointer range for every allocation, not only the first block.
        // LuaJIT 分配器对每次分配强制执行 GC 指针范围约束，而非仅检查第一块。
        let state = NonNull::new(unsafe { ffi::luaL_newstate() })
            .ok_or_else(|| mlua::Error::MemoryError("cannot allocate LuaJIT state".into()))?;
        // Install the owner before any mlua initialization can fail or unwind.
        // 在任何 mlua 初始化可能失败或展开之前建立所有者。
        let mut owner = Self {
            state,
            facade: None,
        };
        // Base globals are independent of StdLib flags and must precede mlua initialization, as in mlua's constructor.
        // 基础全局函数独立于 StdLib 标志，必须与 mlua 构造函数一样，在初始化 mlua 之前加载。
        unsafe {
            ffi::luaL_requiref(state.as_ptr(), c"_G".as_ptr(), ffi::luaopen_base, 1);
            ffi::lua_pop(state.as_ptr(), 1);
        }
        // The external-state API keeps the facade in this exact state's registry until lua_close.
        // 外部状态接口将外观保存在此精确状态的注册表中，直到 lua_close。
        let facade = unsafe { Lua::get_or_init_from_ptr(state.as_ptr()) };
        owner.facade = Some(NonNull::from(facade));
        facade.load_std_libs(libs)?;
        Ok(owner)
    }
}

impl Deref for LuaState {
    type Target = Lua;

    /// Borrow the initialized registry facade for this owner's lifetime; never create another native owner.
    /// 在此所有者生命周期内借用已初始化的注册表外观；绝不创建其他原生所有者。
    fn deref(&self) -> &Lua {
        // Only fully initialized owners can leave new; dereferencing does not touch the raw Lua stack.
        // 仅完整初始化的所有者可离开 new；解引用不会操作原始 Lua 栈。
        unsafe { self.facade.expect("initialized Lua state").as_ref() }
    }
}

impl Drop for LuaState {
    /// Collect live finalizers, close host admission, then destroy the state exactly once; return unit.
    /// 收集存活终结器、关闭宿主准入，然后恰好销毁状态一次；返回空值。
    fn drop(&mut self) {
        if let Some(facade) = self.facade {
            // Borrowing ends before lua_close destroys the registry-owned facade and its weak references.
            // 借用在 lua_close 销毁注册表外观及其弱引用之前结束。
            let lua = unsafe { facade.as_ref() };
            if let Err(error) = lua.gc_collect() {
                crate::runtime::logging::error(format!(
                    "Lua VM finalization before close failed: {error}"
                ));
            }
            lua.set_app_data(Closing);
        }
        // LuaJIT destroys its own allocator here; budget wrappers must have restored it before owner drop.
        // LuaJIT 在此销毁自身分配器；预算包装必须在所有者释放前恢复原分配器。
        unsafe { ffi::lua_close(self.state.as_ptr()) };
    }
}

#[cfg(test)]
mod tests;
