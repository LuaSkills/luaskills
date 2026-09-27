use super::*;
use crate::runtime::embedded::capabilities::ModuleCapabilities;
use crate::runtime::embedded::{
    CallControl, EmbeddedError, EmbeddedErrorCode, EmbeddedResult, JsonContract, ModuleDefinition,
    ModuleInvocation,
};

mod capabilities;

/// One exclusively borrowed VM with immutable, validated function exports.
/// 单个被独占借用且具有不可变已校验函数导出的 VM。
pub struct EmbeddedModule {
    /// Immutable per-module registry snapshot and live permission authority.
    /// 不可变逐模块注册表快照与实时权限权威。
    capabilities: Option<ModuleCapabilities>,
    /// Unique lifecycle operation identity used only during this instance's initialization.
    /// 仅在此实例初始化期间使用的唯一生命周期操作身份。
    initialization_id: String,
    /// Package and directory identities shared with the proven System loader.
    /// 与已验证 System 加载器共享的包和目录身份。
    paths: RuntimeLeasePathContext,
    /// Independently allocated Lua state, never registered in the legacy lease pool.
    /// 独立分配且从不注册到旧租约池的 Lua 状态。
    vm: LuaVm,
    /// Captured functions cannot be redirected by later table-field mutation.
    /// 捕获的函数不能被后续表字段修改重定向。
    exports: BTreeMap<String, CompiledModuleExport>,
    /// Validated contracts consumed by exactly one initialization attempt.
    /// 仅由一次初始化尝试消费的已校验契约。
    pending_contracts: Option<BTreeMap<String, (JsonContract, JsonContract)>>,
    /// Frozen host declaration retained for diagnostics and partition validation.
    /// 为诊断与分区校验保留的冻结宿主声明。
    definition: ModuleDefinition,
    /// Only successful execution and cleanup restore reusability.
    /// 只有执行及清理成功后才恢复可复用状态。
    reusable: bool,
    /// Set only after owned managed resources confirm successful retirement.
    /// 仅在所属受管资源确认退役成功后设置。
    closed: bool,
    /// Drop last so Lua finalizers cannot outlive native libraries or managed services.
    /// 最后释放，确保 Lua 终结器不会比原生库或受管服务存活更久。
    engine: Arc<LuaEngine>,
}

/// Captured function plus immutable input and output contracts.
/// 捕获的函数及不可变输入和输出契约。
#[derive(Clone)]
struct CompiledModuleExport {
    /// Exact VM-bound function captured at activation.
    /// 激活时捕获且绑定 VM 的精确函数。
    function: Function,
    /// Validates arguments before any plugin code can execute.
    /// 在任何插件代码可以执行前校验参数。
    input: JsonContract,
    /// Validates structured results before invocation resources commit.
    /// 在调用资源提交前校验结构化结果。
    output: JsonContract,
}

/// RAII protection removing execution hooks and request control on every exit.
/// 在所有退出路径移除执行钩子与请求控制的 RAII 保护。
struct ModuleBudgetGuard<'a> {
    /// VM exclusively owned by the surrounding invocation.
    /// 由外围调用独占拥有的 VM。
    lua: &'a Lua,
}

impl<'a> ModuleBudgetGuard<'a> {
    /// Install `control` on `lua`; return a guard or a hook-installation failure.
    /// 在 `lua` 上安装 `control`；返回保护对象或钩子安装错误。
    fn install(lua: &'a Lua, control: Arc<CallControl>) -> EmbeddedResult<Self> {
        control.check()?;
        lua.set_app_data(logical_cwd::EvaluationDeadline(control.deadline()));
        lua.set_app_data(Arc::clone(&control));
        // The guard exists before fallible hook installation, so failure removes app data.
        // 在可失败的钩子安装前创建保护对象，确保失败时移除应用数据。
        let guard = Self { lua };
        lua.set_hook(
            HookTriggers::new().every_nth_instruction(LUA_BUDGET_HOOK_INTERVAL),
            move |_, _| {
                control.check().map_err(mlua::Error::external)?;
                Ok(VmState::Continue)
            },
        )
        .map_err(execution_error)?;
        Ok(guard)
    }
}

impl Drop for ModuleBudgetGuard<'_> {
    /// Remove this invocation's hook, deadline, and cancellation reference.
    /// 移除本次调用的钩子、截止时间与取消引用。
    fn drop(&mut self) {
        self.lua.remove_hook();
        self.lua
            .remove_app_data::<logical_cwd::EvaluationDeadline>();
        self.lua.remove_app_data::<Arc<CallControl>>();
        self.lua
            .remove_app_data::<capabilities::CapabilityCallContext>();
    }
}

/// Convert a VM diagnostic `error` without exposing another plugin's state.
/// 转换 VM 诊断 `error`，且不暴露其他插件状态。
fn execution_error(error: impl Display) -> EmbeddedError {
    EmbeddedError::new(EmbeddedErrorCode::ExecutionFailed, error.to_string())
}

impl LuaEngine {
    /// Create a module with immutable `capabilities` installed before source initialization.
    /// 在源码初始化前安装不可变 `capabilities` 并创建模块。
    /// `definition`, `instance_id` and `control` retain the same path and deadline contracts as direct creation.
    /// `definition`、`instance_id` 与 `control` 保持与直接创建相同的路径及截止契约。
    pub fn create_embedded_module_with_capabilities(
        self: &Arc<Self>,
        definition: ModuleDefinition,
        instance_id: &str,
        control: Arc<CallControl>,
        capabilities: ModuleCapabilities,
    ) -> EmbeddedResult<EmbeddedModule> {
        control.check()?;
        // Ownership is established before binding and source execution can fail.
        // 在绑定与源码执行可能失败前建立所有权。
        let mut module = self.allocate_embedded_module(definition, instance_id)?;
        module.bind_capabilities(capabilities)?;
        module.initialize(control)?;
        Ok(module)
    }

    /// Create and initialize `definition` with trusted `instance_id` and original `control`.
    /// 使用可信 `instance_id` 与原始 `control` 创建并初始化 `definition`。
    /// Return a standalone module; governed pools retain failed instances separately.
    /// 返回独立模块；受治理池单独保留失败实例。
    pub fn create_embedded_module(
        self: &Arc<Self>,
        definition: ModuleDefinition,
        instance_id: &str,
        control: Arc<CallControl>,
    ) -> EmbeddedResult<EmbeddedModule> {
        control.check()?;
        // Allocation precedes source execution so governed callers can retain failure ownership.
        // 分配先于源码执行，使受治理调用方可保留失败所有权。
        let mut module = self.allocate_embedded_module(definition, instance_id)?;
        module.initialize(control)?;
        Ok(module)
    }

    /// Allocate `definition` with trusted `instance_id` without executing plugin source.
    /// 使用可信 `instance_id` 分配 `definition`，且不执行插件源码。
    /// Return ownership before initialization so a pool can retain incomplete teardown.
    /// 在初始化前返回所有权，使池可以保留未完成清理。
    pub(crate) fn allocate_embedded_module(
        self: &Arc<Self>,
        definition: ModuleDefinition,
        instance_id: &str,
    ) -> EmbeddedResult<EmbeddedModule> {
        definition.validate()?;
        // Compile value contracts before allocating the VM or running plugin initialization.
        // 在分配 VM 或运行插件初始化前编译值契约。
        let contracts = definition
            .exports
            .iter()
            .map(|export| {
                export
                    .compile()
                    .map(|contracts| (export.name.clone(), contracts))
            })
            .collect::<EmbeddedResult<BTreeMap<_, _>>>()?;
        if instance_id.trim().is_empty() {
            return Err(EmbeddedError::invalid("instance identity must be nonempty"));
        }
        // Reuse exactly the trusted package and logical-directory resolution contract.
        // 精确复用可信包与逻辑目录解析契约。
        let request = SystemRuntimeSessionCreateRequest {
            sid: instance_id.to_owned(),
            ttl_sec: Some(0),
            replace: false,
            cwd: definition.cwd.clone(),
            workspace_root: definition.workspace_root.clone(),
            mounts: definition.mounts.clone(),
            system_package: SystemRuntimePackageRequest {
                id: definition.plugin_id.clone(),
                root: definition.package_root.clone(),
                dependencies_file: definition.dependencies_file.clone(),
            },
        };
        // No VM or callback is allocated until path identities have been established.
        // 在路径身份确立前不分配 VM 或回调。
        let paths = self
            .resolve_system_runtime_lease_path_context(&request)
            .map_err(execution_error)?;
        // This exact package is required by the System path resolver, never guessed.
        // 此精确包由 System 路径解析器强制提供，不进行猜测。
        let package = paths.managed_package.as_ref().ok_or_else(|| {
            EmbeddedError::new(
                EmbeddedErrorCode::Internal,
                "System package binding is missing",
            )
        })?;
        package
            .lease_binding()
            .ok_or_else(|| {
                EmbeddedError::new(
                    EmbeddedErrorCode::Internal,
                    "System lease binding is missing",
                )
            })?
            .bind(instance_id.to_owned(), 1)
            .map_err(execution_error)?;
        // Each instance has one immutable incarnation; package generation is tracked separately.
        // 每个实例只有一个不可变生命周期；包代次独立记录。
        let vm = self.create_system_runtime_vm().map_err(execution_error)?;
        Self::configure_runtime_lease_vm(&vm.lua, &paths).map_err(execution_error)?;
        logical_cwd::install(
            &vm.lua,
            paths.cwd.as_deref().ok_or_else(|| {
                EmbeddedError::new(
                    EmbeddedErrorCode::Internal,
                    "System logical directory is missing",
                )
            })?,
            resolve_host_default_text_encoding(self.host_options.as_ref())
                .map_err(execution_error)?,
        )
        .map_err(execution_error)?;
        // Construct the owner before running source so every failure retires created resources.
        // 在运行源码前构造所有者，确保所有失败路径退役已创建资源。
        capabilities::install(&vm.lua, None).map_err(execution_error)?;
        Ok(EmbeddedModule {
            capabilities: None,
            initialization_id: format!("{instance_id}:initialize"),
            engine: Arc::clone(self),
            paths,
            vm,
            exports: BTreeMap::new(),
            pending_contracts: Some(contracts),
            definition,
            reusable: false,
            closed: false,
        })
    }
}

impl EmbeddedModule {
    /// Bind `capabilities` exactly once before initialization; reject replacing a live VM's authority.
    /// 在初始化前精确绑定一次 `capabilities`；拒绝替换活动 VM 权威。
    pub(crate) fn bind_capabilities(
        &mut self,
        capabilities: ModuleCapabilities,
    ) -> EmbeddedResult<()> {
        if self.pending_contracts.is_none() || self.capabilities.is_some() {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Closed,
                "module capability binding is already frozen",
            ));
        }
        capabilities::install(&self.vm.lua, Some(capabilities.clone())).map_err(execution_error)?;
        self.capabilities = Some(capabilities);
        Ok(())
    }

    /// Execute source with `control` and consume the already-compiled contracts once.
    /// 使用 `control` 执行源码，并且仅消费一次已编译契约。
    /// Return only after initialization resources have committed successfully.
    /// 仅在初始化资源成功提交后返回。
    pub(crate) fn initialize(&mut self, control: Arc<CallControl>) -> EmbeddedResult<()> {
        // Consume contracts before execution to prevent replay after partial effects.
        // 在执行前消费契约，防止部分副作用后重放初始化。
        let contracts = self.pending_contracts.take().ok_or_else(|| {
            EmbeddedError::new(
                EmbeddedErrorCode::Closed,
                "module initialization was already attempted",
            )
        })?;
        // Initialization has an empty request context, never a previous caller's metadata.
        // 初始化使用空请求上下文，绝不使用上一个调用方的元数据。
        let context = LuaInvocationContext::default();
        // Capture immutable source so execution does not borrow mutable module metadata.
        // 捕获不可变源码，避免执行借用可变模块元数据。
        let source = self.definition.source.clone();
        // Resolve exact functions only after every value contract has compiled successfully.
        // 仅在全部值契约编译成功后解析精确函数。
        let initialization_id = self.initialization_id.clone();
        let exports = self.run(&context, control, &initialization_id, None, |lua| {
            // The module return shape is fixed by the declared runtime protocol.
            // 模块返回形状由声明的运行时协议固定。
            let table: Table = lua.load(&source).set_name("embedded_module").eval()?;
            contracts
                .into_iter()
                .map(|(name, (input, output))| {
                    table.raw_get::<Function>(name.as_str()).map(|function| {
                        (
                            name,
                            CompiledModuleExport {
                                function,
                                input,
                                output,
                            },
                        )
                    })
                })
                .collect::<mlua::Result<BTreeMap<_, _>>>()
        })?;
        self.exports = exports;
        Ok(())
    }

    /// Invoke the exact function and values in `invocation` without compiling Lua source.
    /// 使用 `invocation` 中的精确函数与值调用，不编译 Lua 源码。
    /// Return structured JSON; failed execution or cleanup prevents subsequent reuse.
    /// 返回结构化 JSON；执行或清理失败会阻止后续复用。
    pub fn invoke(&mut self, invocation: ModuleInvocation<'_>) -> EmbeddedResult<Value> {
        if self.closed || !self.reusable {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Closed,
                "module is closed or requires retirement",
            ));
        }
        // Function and schemas are captured together from the exact activation declaration.
        // 函数与 Schema 从精确激活声明中一同捕获。
        let export = self
            .exports
            .get(invocation.export)
            .cloned()
            .ok_or_else(|| {
                EmbeddedError::new(EmbeddedErrorCode::NotFound, "module export is not declared")
            })?;
        export.input.validate(invocation.arguments)?;
        self.run(
            invocation.context,
            invocation.control,
            invocation.operation_id,
            invocation.session_id,
            |lua| {
                // JSON conversion preserves the canonical null and empty-container representation.
                // JSON 转换保留规范空值与空容器表示。
                let argument = lua.to_value(invocation.arguments)?;
                // Direct function calls do not compile a new wrapper for every request.
                // 直接函数调用不为每次请求编译新包装。
                let result = export.function.call::<LuaValue>(argument)?;
                // Output contract validation happens before request-owned resources commit.
                // 输出契约在请求所属资源提交前校验。
                let value = lua.from_value(result)?;
                export
                    .output
                    .validate(&value)
                    .map_err(mlua::Error::external)?;
                Ok(value)
            },
        )
    }

    /// Run `execute` under trusted `context` and the original `control` budget.
    /// 在可信 `context` 与原始 `control` 预算下运行 `execute`。
    /// Return its value after request cleanup, or retain a non-reusable failed instance.
    /// 在请求清理后返回结果，否则保留不可复用的失败实例。
    fn run<T>(
        &mut self,
        context: &LuaInvocationContext,
        control: Arc<CallControl>,
        operation_id: &str,
        session_id: Option<&str>,
        execute: impl FnOnce(&Lua) -> mlua::Result<T>,
    ) -> EmbeddedResult<T> {
        self.reusable = false;
        control.check()?;
        self.paths
            .validate_live_system_directory_identities()
            .map_err(execution_error)?;
        // A System module always owns this validated package context.
        // System 模块始终拥有此已校验包上下文。
        let package = self.paths.managed_package.as_ref().ok_or_else(|| {
            EmbeddedError::new(EmbeddedErrorCode::Internal, "module package disappeared")
        })?;
        package
            .validate_live_filesystem_identity()
            .map_err(execution_error)?;
        // Failed work rolls back resources created within this invocation.
        // 失败工作回滚本次调用内创建的资源。
        let transaction = self
            .engine
            .managed_runtime_services
            .begin_transaction(package.owner_token())
            .map_err(execution_error)?;
        // Restore transaction app data even during an unwinding callback failure.
        // 即使回调失败展开栈，也恢复事务应用数据。
        let _transaction_scope = LuaManagedRuntimeTransactionScopeGuard::install(
            &self.vm.lua,
            Some(transaction.context()),
        );
        reset_pooled_vm_request_scope(&self.vm.lua, self.engine.host_options.as_ref())
            .map_err(execution_error)?;
        LuaEngine::populate_anonymous_lua_context(
            &self.vm.lua,
            AnonymousLuaExecutionContext {
                invocation_context: Some(context),
                internal_context: VulcanInternalExecutionContext {
                    luaexec_active: true,
                    ..VulcanInternalExecutionContext::default()
                },
                entry_file: None,
                dependency_context: AnonymousLuaDependencyContext::ClearWithHostOptions(
                    self.engine.host_options.as_ref(),
                ),
                managed_package_context: AnonymousLuaManagedPackageContext::Set(package),
            },
        )
        .map_err(execution_error)?;
        populate_system_plugin_runtime_context(&self.vm.lua, Some(package))
            .map_err(execution_error)?;
        // One guard protects initialization, execution, and JSON result conversion alike.
        // 同一保护对象覆盖初始化、执行与 JSON 结果转换。
        let guard = ModuleBudgetGuard::install(&self.vm.lua, Arc::clone(&control))?;
        // Derive caller authority only from frozen host declarations and explicit call identifiers.
        // 仅从冻结宿主声明与显式调用标识派生调用方权威。
        let caller = self
            .capabilities
            .as_ref()
            .map(|binding| {
                binding.caller(
                    &self.definition,
                    operation_id.to_owned(),
                    session_id.map(str::to_owned),
                )
            })
            .transpose()?;
        self.vm
            .lua
            .set_app_data(capabilities::CapabilityCallContext {
                caller,
                control: Arc::clone(&control),
            });
        // Save the result before unconditional request-context cleanup.
        // 在无条件清理请求上下文前保存执行结果。
        let result = execute(&self.vm.lua);
        drop(guard);
        reset_pooled_vm_request_scope(&self.vm.lua, self.engine.host_options.as_ref())
            .map_err(execution_error)?;
        control.check()?;
        // Preserve execution failure before committing invocation-owned child resources.
        // 在提交调用拥有的子资源前保留执行失败。
        let result = result.map_err(execution_error)?;
        transaction.commit().map_err(execution_error)?;
        self.reusable = true;
        Ok(result)
    }

    /// Return the immutable activation declaration for host-side identity checks.
    /// 返回不可变激活声明，供宿主侧身份检查使用。
    pub fn definition(&self) -> &ModuleDefinition {
        &self.definition
    }

    /// Report whether successful execution and cleanup permit another call.
    /// 报告执行及清理成功后是否允许再次调用。
    pub fn is_reusable(&self) -> bool {
        self.reusable && !self.closed
    }

    /// Retire owned processes and workers; retain this instance on cleanup failure.
    /// 退役所属进程与工作器；清理失败时保留此实例。
    /// Return success only when the module may be removed from capacity accounting.
    /// 仅在可从容量账本移除此模块时返回成功。
    pub fn close(&mut self) -> EmbeddedResult<()> {
        if self.closed {
            return Ok(());
        }
        self.reusable = false;
        // The package owner is unique to this VM and cannot retire another instance.
        // 包所有者由此 VM 独占，无法退役其他实例。
        let package = self.paths.managed_package.as_ref().ok_or_else(|| {
            EmbeddedError::new(EmbeddedErrorCode::Internal, "module package disappeared")
        })?;
        retire_managed_runtime_owner_state(package.owner_token());
        self.engine
            .retire_managed_runtime_owner(package.owner_token())
            .map_err(|message| EmbeddedError::new(EmbeddedErrorCode::CleanupFailed, message))?;
        self.closed = true;
        Ok(())
    }
}

impl Drop for EmbeddedModule {
    /// Attempt owned-resource retirement; the existing retry service retains failed cleanup.
    /// 尝试退役所属资源；既有重试服务保留失败的清理。
    fn drop(&mut self) {
        if let Err(error) = self.close() {
            log_error(format!("embedded module retirement pending: {error}"));
        }
    }
}
