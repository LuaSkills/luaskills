//! Lua VM close: finalizers run while the instance is alive, and host functions are refused once it closes.
//! Lua 虚拟机关闭：终结器在实例存活时运行，实例关闭后拒绝宿主函数。

use super::*;
use mlua::{FromLuaMulti, IntoLuaMulti, MaybeSend, UserDataMethods};

/// Error raised in Lua when a finalizer calls a host function while `lua_close` runs.
/// 终结器在 `lua_close` 期间调用宿主函数时于 Lua 中抛出的错误。
pub(crate) const HOST_CALL_WHILE_CLOSING: &str = "host functions are unavailable while the Lua VM is closing; release host resources before the VM closes, for example in the declared module finalizer export";

/// Run pending finalizers on a live `lua` before the last handle drops and `lua_close` runs.
/// 在最后一个句柄释放、`lua_close` 运行之前，于存活的 `lua` 上运行待执行终结器。
#[cfg(test)]
fn prepare_lua_close(lua: &Lua) {
    if let Err(error) = lua.gc_collect() {
        log_error(format!("Lua VM finalization before close failed: {error}"));
    }
}

/// Refuse a host call once the instance is closing.
/// 实例关闭后拒绝宿主调用。
/// Core-owned states close admission before `lua_close`; registry destruction then invalidates mlua handles.
/// 核心状态在 `lua_close` 前关闭准入；注册表销毁随后使 mlua 句柄失效。
/// Refuse the call before converting arguments or touching a Lua value in either closing phase.
/// 在关闭的任一阶段，都必须在转换参数或访问 Lua 值之前拒绝调用。
/// LuaJIT disables hooks while finalizers run, so this check cannot be a call hook.
/// LuaJIT 在终结器运行期间禁用钩子，因此该检查不能用调用钩子实现。
fn ensure_open(lua: &Lua) -> mlua::Result<()> {
    // Owned mlua states lose their weak owner at close; external core states additionally mark admission closed.
    // mlua 自有状态在关闭时失去弱所有者；核心外部状态还会显式标记准入关闭。
    match lua.weak().try_upgrade() {
        Some(_)
            if lua
                .app_data_ref::<crate::runtime::lua_state::Closing>()
                .is_none() =>
        {
            Ok(())
        }
        _ => Err(mlua::Error::runtime(HOST_CALL_WHILE_CLOSING)),
    }
}

/// Host function constructors that refuse calls while the VM closes; the only allowed way to create Rust functions.
/// 在 VM 关闭期间拒绝调用的宿主函数构造器；创建 Rust 函数的唯一允许方式。
pub(crate) trait HostFunctions {
    /// Same as `Lua::create_function`, refusing calls while the VM closes.
    /// 与 `Lua::create_function` 相同，但在 VM 关闭期间拒绝调用。
    fn create_host_function<F, A, R>(&self, func: F) -> mlua::Result<Function>
    where
        F: Fn(&Lua, A) -> mlua::Result<R> + MaybeSend + 'static,
        A: FromLuaMulti,
        R: IntoLuaMulti;

    /// Same as `Lua::create_function_mut`, refusing calls while the VM closes.
    /// 与 `Lua::create_function_mut` 相同，但在 VM 关闭期间拒绝调用。
    fn create_host_function_mut<F, A, R>(&self, func: F) -> mlua::Result<Function>
    where
        F: FnMut(&Lua, A) -> mlua::Result<R> + MaybeSend + 'static,
        A: FromLuaMulti,
        R: IntoLuaMulti;
}

#[allow(clippy::disallowed_methods)]
impl HostFunctions for Lua {
    fn create_host_function<F, A, R>(&self, func: F) -> mlua::Result<Function>
    where
        F: Fn(&Lua, A) -> mlua::Result<R> + MaybeSend + 'static,
        A: FromLuaMulti,
        R: IntoLuaMulti,
    {
        // Arguments stay raw until the gate passes, because converting them can touch Lua values.
        // 参数在闸门通过前保持原始形式，因为转换参数可能访问 Lua 值。
        self.create_function(move |lua, args: MultiValue| {
            ensure_open(lua)?;
            func(lua, A::from_lua_args(args, 1, None, lua)?)
        })
    }

    fn create_host_function_mut<F, A, R>(&self, mut func: F) -> mlua::Result<Function>
    where
        F: FnMut(&Lua, A) -> mlua::Result<R> + MaybeSend + 'static,
        A: FromLuaMulti,
        R: IntoLuaMulti,
    {
        self.create_function_mut(move |lua, args: MultiValue| {
            ensure_open(lua)?;
            func(lua, A::from_lua_args(args, 1, None, lua)?)
        })
    }
}

/// Userdata method registration that refuses calls while the VM closes; the only allowed way to add methods.
/// 在 VM 关闭期间拒绝调用的 userdata 方法注册；添加方法的唯一允许方式。
pub(crate) trait HostMethods<T> {
    /// Register a function receiving explicit userdata, checking VM lifetime before argument conversion.
    /// 注册接收显式 userdata 的函数，在转换参数前检查虚拟机生命周期。
    /// `name` identifies the method and `function` returns its Lua results or error from the supplied arguments.
    /// `name` 标识方法，`function` 根据传入参数返回 Lua 结果或错误。
    #[cfg(windows)]
    fn add_host_function<F, A, R>(&mut self, name: impl Into<String>, function: F)
    where
        F: Fn(&Lua, A) -> mlua::Result<R> + MaybeSend + 'static,
        A: FromLuaMulti,
        R: IntoLuaMulti;

    /// Same as `UserDataMethods::add_method`, refusing calls while the VM closes.
    /// 与 `UserDataMethods::add_method` 相同，但在 VM 关闭期间拒绝调用。
    fn add_host_method<M, A, R>(&mut self, name: impl Into<String>, method: M)
    where
        M: Fn(&Lua, &T, A) -> mlua::Result<R> + MaybeSend + 'static,
        A: FromLuaMulti,
        R: IntoLuaMulti;

    /// Same as `UserDataMethods::add_meta_method`, refusing calls while the VM closes.
    /// 与 `UserDataMethods::add_meta_method` 相同，但在 VM 关闭期间拒绝调用。
    fn add_host_meta_method<M, A, R>(&mut self, name: impl Into<String>, method: M)
    where
        M: Fn(&Lua, &T, A) -> mlua::Result<R> + MaybeSend + 'static,
        A: FromLuaMulti,
        R: IntoLuaMulti;
}

#[allow(clippy::disallowed_methods)]
impl<T, U: UserDataMethods<T>> HostMethods<T> for U {
    /// Install the explicit-userdata function through the same close gate as ordinary methods.
    /// 通过与普通方法相同的关闭闸门安装显式 userdata 函数。
    /// `name` labels argument errors; `function` owns the implementation; returns unit after registration.
    /// `name` 标识参数错误；`function` 拥有实现；注册后返回空值。
    #[cfg(windows)]
    fn add_host_function<F, A, R>(&mut self, name: impl Into<String>, function: F)
    where
        F: Fn(&Lua, A) -> mlua::Result<R> + MaybeSend + 'static,
        A: FromLuaMulti,
        R: IntoLuaMulti,
    {
        // The label is retained until argument conversion has passed the lifetime gate.
        // 标签保留到参数转换通过生命周期闸门之后。
        let label = name.into();
        self.add_function(label.clone(), move |lua, args: MultiValue| {
            ensure_open(lua)?;
            function(lua, A::from_lua_args(args, 1, Some(&label), lua)?)
        });
    }

    fn add_host_method<M, A, R>(&mut self, name: impl Into<String>, method: M)
    where
        M: Fn(&Lua, &T, A) -> mlua::Result<R> + MaybeSend + 'static,
        A: FromLuaMulti,
        R: IntoLuaMulti,
    {
        let name = name.into();
        let label = name.clone();
        self.add_method(name, move |lua, this, args: MultiValue| {
            ensure_open(lua)?;
            method(lua, this, A::from_lua_args(args, 2, Some(&label), lua)?)
        });
    }

    fn add_host_meta_method<M, A, R>(&mut self, name: impl Into<String>, method: M)
    where
        M: Fn(&Lua, &T, A) -> mlua::Result<R> + MaybeSend + 'static,
        A: FromLuaMulti,
        R: IntoLuaMulti,
    {
        let name = name.into();
        let label = name.clone();
        self.add_meta_method(name, move |lua, this, args: MultiValue| {
            ensure_open(lua)?;
            method(lua, this, A::from_lua_args(args, 2, Some(&label), lua)?)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// Unreachable finalizers still reach host functions before close; finalizers run by `lua_close` get the closing error.
    /// 不可达终结器在关闭前仍可进入宿主函数；`lua_close` 运行的终结器得到关闭错误。
    #[test]
    fn lua_close_finalizers_never_enter_host_functions() {
        let calls = Arc::new(AtomicUsize::new(0));
        let refused = Arc::new(Mutex::new(Vec::<String>::new()));
        let lua = crate::runtime::lua_state::LuaState::new(mlua::StdLib::ALL_SAFE).unwrap();
        let counter = Arc::clone(&calls);
        lua.globals()
            .set(
                "touch",
                lua.create_host_function(move |_, label: String| {
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(label)
                })
                .unwrap(),
            )
            .unwrap();
        // The recorder reads its string straight from the stack, so it observes the error without a gate.
        // 记录器直接从栈读取字符串，因此无需闸门即可观察错误。
        let errors = Arc::clone(&refused);
        #[allow(clippy::disallowed_methods)]
        let record = lua
            .create_function(move |_, error: String| {
                errors.lock().unwrap().push(error);
                Ok(())
            })
            .unwrap();
        lua.globals().set("record", record).unwrap();
        lua.load(
            r#"
            collectgarbage('stop')
            local function tracked(label)
                local proxy = newproxy(true)
                getmetatable(proxy).__gc = function()
                    local ok, err = pcall(touch, label)
                    if not ok then record(tostring(err)) end
                end
                return proxy
            end
            retained = tracked('closing')
            tracked('live')
        "#,
        )
        .exec()
        .unwrap();
        prepare_lua_close(&lua);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        drop(lua);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let refused = refused.lock().unwrap();
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert!(refused[0].contains(HOST_CALL_WHILE_CLOSING), "{refused:?}");
    }
}
