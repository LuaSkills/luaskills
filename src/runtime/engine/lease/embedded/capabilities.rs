use super::*;
use crate::runtime::embedded::EffectState;
use crate::runtime::embedded::capabilities::{
    CapabilityCaller, CapabilityOutcome, ModuleCapabilities,
};

/// Actual host-selected Lua execution stage; source and arguments cannot choose callback authority.
/// 真实宿主选择的 Lua 执行阶段；源码和参数不能选择回调权威。
#[derive(Clone, Copy)]
pub(super) enum CapabilityCallPhase {
    /// Evaluate the module source before any export is captured.
    /// 在捕获任何导出之前求值模块源码。
    Initialization,
    /// Invoke one captured business or closing export.
    /// 调用单个已捕获业务或关闭导出。
    Export,
}

impl CapabilityCallPhase {
    /// Check exact `name` against `binding` for this actual stage; grants remain checked by the registry.
    /// 针对此真实阶段的 `binding` 检查精确 `name`；注册表仍检查授权。
    /// Return whether the stage permits discovery or dispatch, without running a callback.
    /// 返回此阶段是否允许发现或分发，不执行回调。
    fn permits(self, binding: &ModuleCapabilities, name: &str) -> bool {
        match self {
            Self::Initialization => binding.allows_initialization(name),
            Self::Export => true,
        }
    }
}

/// Host-only per-invocation authority; Lua receives results, never a mutable reference to this slot.
/// 仅宿主可写的逐调用权威；Lua 仅接收结果，绝不获得此槽位的可变引用。
#[derive(Clone)]
pub(super) struct CapabilityCallContext {
    /// Caller derived from the immutable module definition, absent for unbound modules.
    /// 从不可变模块定义派生的调用方，未绑定模块省略。
    pub(super) caller: Option<CapabilityCaller>,
    /// Original operation deadline and cancellation authority.
    /// 原始操作截止时间与取消权威。
    pub(super) control: Arc<CallControl>,
    /// Actual execution stage installed immediately before Lua runs and cleared by the same budget guard.
    /// 在 Lua 运行前立即安装并由同一预算保护对象清除的真实执行阶段。
    pub(super) phase: CapabilityCallPhase,
}

/// Install the module's capability facade before any plugin source can retain legacy global callbacks.
/// 在任何插件源码可以保留旧全局回调前安装模块能力外观。
/// `binding` is immutable for the VM lifetime; absence grants no host callbacks.
/// `binding` 在 VM 生命周期内不可变；省略时不授予宿主回调。
pub(super) fn install(lua: &Lua, binding: Option<ModuleCapabilities>) -> mlua::Result<()> {
    // The canonical root is constructed by create_system_runtime_vm before module initialization.
    // 规范根由 create_system_runtime_vm 在模块初始化前构造。
    let vulcan: Table = lua.globals().get("vulcan")?;
    // One table provides both the new name and the host-call compatibility facade.
    // 单个表同时提供新名称与宿主调用兼容外观。
    let capabilities = lua.create_table()?;
    // Discovery captures the same immutable registration snapshot as execution.
    // 发现捕获与执行相同的不可变注册快照。
    let discovery = binding.clone();
    capabilities.set(
        "list",
        lua.create_function(move |lua, ()| {
            // Unbound modules expose an empty capability set, never process-global callback state.
            // 未绑定模块暴露空能力集合，绝不暴露进程全局回调状态。
            let context = lua
                .app_data_ref::<CapabilityCallContext>()
                .map(|context| context.clone());
            let descriptors = match (&discovery, context) {
                (Some(binding), Some(context)) => binding
                    .snapshot
                    .list(&binding.permissions)
                    .map_err(mlua::Error::external)?
                    .into_iter()
                    .filter(|descriptor| context.phase.permits(binding, &descriptor.name))
                    .collect(),
                _ => Vec::new(),
            };
            lua.to_value(&descriptors)
        })?,
    )?;
    // Existence checks are permission-filtered through the same registry authority.
    // 存在性检查通过相同注册表权威进行权限过滤。
    let existence = binding.clone();
    let has = lua.create_function(move |lua, name: String| {
        // A retained Lua closure reads the current host stage on every use, not the stage when it was captured.
        // 保留的 Lua 闭包在每次使用时读取当前宿主阶段，而非其被捕获时的阶段。
        let context = lua
            .app_data_ref::<CapabilityCallContext>()
            .map(|context| context.clone());
        match (&existence, context) {
            (Some(binding), Some(context)) if context.phase.permits(binding, &name) => binding
                .snapshot
                .has(&name, &binding.permissions)
                .map_err(mlua::Error::external),
            _ => Ok(false),
        }
    })?;
    capabilities.set("has", has.clone())?;
    capabilities.set("has_tool", has)?;
    capabilities.set(
        "call",
        lua.create_function(move |lua, (name, arguments): (String, LuaValue)| {
            // Copy app data before any callback can block; no Lua app-data borrow crosses a host wait.
            // 在任何回调可能阻塞前复制应用数据；宿主等待期间不持有 Lua 应用数据借用。
            let context = lua
                .app_data_ref::<CapabilityCallContext>()
                .map(|context| context.clone());
            // Failures before dispatch are known to have produced no host side effect.
            // 分发前失败可以确定未产生宿主副作用。
            let result = match (&binding, context) {
                (Some(binding), Some(context)) => match context.caller {
                    Some(_) if !context.phase.permits(binding, &name) => Err(EmbeddedError::new(
                        EmbeddedErrorCode::PermissionDenied,
                        "capability is not authorized during module initialization",
                    )),
                    Some(caller) => values::callback_arguments(&arguments).and_then(|arguments| {
                        binding.snapshot.invoke(
                            &name,
                            caller,
                            Arc::clone(&binding.permissions),
                            arguments,
                            context.control,
                        )
                    }),
                    None => Err(unbound()),
                },
                _ => Err(unbound()),
            };
            // The shared envelope preserves effect evidence even when the result is an error.
            // 共享信封即使在结果为错误时也保留副作用证据。
            let mut outcome = result.unwrap_or_else(|error| CapabilityOutcome {
                result: Err(error),
                effects: EffectState::NotStarted,
            });
            // Reject an unsafe host JSON integer only after the registry has retained real effect evidence.
            // 仅在注册表保留真实副作用证据后拒绝不安全宿主 JSON 整数。
            // A failed Lua representation changes the response, never the host's confirmed effect state.
            // Lua 表示失败改变响应，绝不改变宿主确认的副作用状态。
            // This fixed host root keeps the diagnostic bounded after the registry's output-budget check.
            // 此固定宿主根确保注册表输出预算检查后的诊断保持有界。
            if let Ok(value) = &outcome.result
                && let Err(error) =
                    LuaEngine::validate_embedded_json_value(value, "capability/value")
            {
                outcome.result = Err(error);
            }
            values::to_lua(lua, &outcome.to_json(), "capability")
        })?,
    )?;
    vulcan.raw_set("capabilities", capabilities.clone())?;
    vulcan.raw_set("host", capabilities)?;
    // Legacy model and management bridges otherwise reach process-global callbacks outside this registry.
    // 否则旧模型与管理桥接会访问此注册表之外的进程全局回调。
    vulcan.raw_set("models", lua.create_table()?)?;
    let runtime: Table = vulcan.raw_get("runtime")?;
    let skills = lua.create_table()?;
    skills.raw_set("enabled", false)?;
    runtime.raw_set("skills", skills)?;
    Ok(())
}

/// Return a fixed unbound-context diagnostic without revealing another runtime's registrations.
/// 返回固定未绑定上下文诊断，不暴露其他运行时的注册。
fn unbound() -> EmbeddedError {
    EmbeddedError::new(
        EmbeddedErrorCode::PermissionDenied,
        "module has no active capability authority",
    )
}
