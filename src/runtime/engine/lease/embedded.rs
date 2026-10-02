use super::*;
use crate::runtime::embedded::PreparedModuleDefinition;
use crate::runtime::embedded::capabilities::ModuleCapabilities;
use crate::runtime::embedded::{
    CallControl, EmbeddedError, EmbeddedErrorCode, EmbeddedResult, JsonContract, ModuleDefinition,
    ModuleInvocation,
};

mod capabilities;
mod paths;
mod values;

/// One exclusively borrowed VM with immutable, validated function exports.
/// 单个被独占借用且具有不可变已校验函数导出的 VM。
pub struct EmbeddedModule {
    /// Stage-local measurement flag set by its exclusive lease outside runtime locks.
    /// 由其独占租借在运行时锁外设置的阶段局部测量标志。
    diagnostic_measurement_enabled: bool,
    /// Actual Lua compilation and source evaluation interval for this initialization attempt only.
    /// 仅此初始化尝试的实际 Lua 编译及源码求值区间。
    diagnostic_bundle_evaluation_elapsed: Option<std::time::Duration>,
    /// Actual post-execution request-scope reset call interval, absent when that cleanup never entered.
    /// 实际执行后请求作用域重置调用区间；未进入该清理时省略。
    diagnostic_request_cleanup_elapsed: Option<std::time::Duration>,
    /// Optional actual VM creation interval, captured only for the subscribed formal allocation.
    /// 可选实际 VM 创建区间，仅为已订阅正式分配捕获。
    diagnostic_vm_creation_elapsed: Option<std::time::Duration>,
    /// Immutable per-module registry snapshot and live permission authority.
    /// 不可变逐模块注册表快照与实时权限权威。
    capabilities: Option<ModuleCapabilities>,
    /// Unique lifecycle operation identity used only during this instance's initialization.
    /// 仅在此实例初始化期间使用的唯一生命周期操作身份。
    initialization_id: String,
    /// Package and directory identities shared with the proven System loader.
    /// 与已验证 System 加载器共享的包和目录身份。
    paths: RuntimeLeasePathContext,
    /// Drop captured roots before the primary Lua owner runs its final live garbage collection.
    /// 在主 Lua 所有者执行最后一次存活垃圾回收前释放捕获根。
    exports: BTreeMap<String, CompiledModuleExport>,
    /// Independently allocated Lua state; destruction follows release of captured export roots.
    /// 独立分配的 Lua 状态；销毁发生在捕获导出根释放之后。
    vm: LuaVm,
    /// Validated contracts consumed by exactly one initialization attempt.
    /// 仅由一次初始化尝试消费的已校验契约。
    pending_contracts: Option<BTreeMap<String, (JsonContract, JsonContract)>>,
    /// Original declaration and compiled contracts retained for diagnostics and partition validation.
    /// 为诊断与分区校验保留的原声明及已编译契约。
    prepared: Arc<PreparedModuleDefinition>,
    /// Only successful execution and cleanup restore reusability.
    /// 只有执行及清理成功后才恢复可复用状态。
    reusable: bool,
    /// Claimed before any explicit closing-export attempt; never reset by success, errors or unwinding.
    /// 在任何显式关闭导出尝试前认领；成功、错误或栈展开均不重置。
    finalization_started: bool,
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
        self.allocate_prepared_embedded_module(
            PreparedModuleDefinition::new(definition)?,
            instance_id,
        )
    }

    /// Allocate prepared's exact declaration with fresh instance_id and unchanged live path validation.
    /// 使用新 instance_id 分配 prepared 的精确声明，保持实时路径校验不变。
    /// Return real VM ownership before initialization; shared contracts grant no filesystem or capability authority.
    /// 初始化前返回真实 VM 所有权；共享契约不授予文件系统或能力权威。
    pub(crate) fn allocate_prepared_embedded_module(
        self: &Arc<Self>,
        prepared: Arc<PreparedModuleDefinition>,
        instance_id: &str,
    ) -> EmbeddedResult<EmbeddedModule> {
        self.allocate_prepared_embedded_module_measured(prepared, instance_id, false)
    }

    /// Allocate prepared for instance_id, optionally measuring only the actual VM construction call.
    /// 为 instance_id 分配 prepared，可选仅测量实际 VM 构造调用。
    /// measure_vm_creation was resolved outside runtime locks; return ownership without logging on the VM stack.
    /// measure_vm_creation 在运行时锁外解析；返回所有权，不在 VM 栈内记录日志。
    pub(crate) fn allocate_prepared_embedded_module_measured(
        self: &Arc<Self>,
        prepared: Arc<PreparedModuleDefinition>,
        instance_id: &str,
        measure_vm_creation: bool,
    ) -> EmbeddedResult<EmbeddedModule> {
        if instance_id.trim().is_empty() {
            return Err(EmbeddedError::invalid("instance identity must be nonempty"));
        }
        // No VM or callback is allocated until path identities have been established.
        // 在路径身份确立前不分配 VM 或回调。
        let paths = self
            .resolve_embedded_module_paths(prepared.definition(), instance_id)
            .map_err(execution_error)?;
        // Each instance has one immutable incarnation; package generation is tracked separately.
        // 每个实例只有一个不可变生命周期；包代次独立记录。
        // Disabled diagnostics never read a clock for this construction interval.
        // 关闭诊断时绝不为此构造区间读取时钟。
        let vm_creation_started = measure_vm_creation.then(std::time::Instant::now);
        let vm = self.create_system_runtime_vm().map_err(execution_error)?;
        // Keep the scalar for emission after the surrounding allocation has returned and all locks are released.
        // 保留标量，在外围分配返回且全部锁释放后发送。
        let diagnostic_vm_creation_elapsed = vm_creation_started.map(|started| started.elapsed());
        // LuaJIT traces can bypass instruction hooks; disable the engine before any plugin code loads.
        // LuaJIT 跟踪代码可能绕过指令钩子；在任何插件源码加载前禁用编译引擎。
        vm.lua
            .load("jit.off(); jit.flush()")
            .set_name("embedded_execution_budget")
            .exec()
            .map_err(execution_error)?;
        Self::configure_runtime_lease_vm(&vm.lua, &paths).map_err(execution_error)?;
        logical_cwd::install(
            &vm.lua,
            paths.cwd.as_deref().ok_or_else(|| {
                EmbeddedError::new(
                    EmbeddedErrorCode::Internal,
                    "module logical directory is missing",
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
            diagnostic_measurement_enabled: false,
            diagnostic_bundle_evaluation_elapsed: None,
            diagnostic_request_cleanup_elapsed: None,
            diagnostic_vm_creation_elapsed,
            capabilities: None,
            initialization_id: format!("{instance_id}:initialize"),
            engine: Arc::clone(self),
            paths,
            vm,
            exports: BTreeMap::new(),
            pending_contracts: Some(prepared.contracts().clone()),
            prepared,
            reusable: false,
            finalization_started: false,
            closed: false,
        })
    }
}

impl EmbeddedModule {
    /// Begin an optional measured stage and clear previous observations before reused invocation.
    /// 开始可选被测阶段，并在复用调用前清除之前的观测。
    /// enabled was resolved outside runtime locks; this method does not query logging or Lua.
    /// enabled 在运行时锁外解析；此方法不查询日志或 Lua。
    pub(crate) fn begin_diagnostic_stage(&mut self, enabled: bool) {
        self.diagnostic_measurement_enabled = enabled;
        self.diagnostic_bundle_evaluation_elapsed = None;
        self.diagnostic_request_cleanup_elapsed = None;
    }

    /// Return this stage's real compilation and evaluation interval, absent when source was not entered.
    /// 返回此阶段的真实编译及求值区间；未进入源码时省略。
    pub(crate) fn diagnostic_bundle_evaluation_elapsed(&self) -> Option<std::time::Duration> {
        self.diagnostic_bundle_evaluation_elapsed
    }

    /// Return this stage's actual post-execution request reset interval, including returned cleanup errors.
    /// 返回此阶段的实际执行后请求重置区间，包括返回的清理错误。
    pub(crate) fn diagnostic_request_cleanup_elapsed(&self) -> Option<std::time::Duration> {
        self.diagnostic_request_cleanup_elapsed
    }
    /// Read the measured allocation scalar without calling the logger or executing Lua.
    /// 读取已测分配标量，不调用日志器或执行 Lua。
    pub(crate) fn diagnostic_vm_creation_elapsed(&self) -> Option<std::time::Duration> {
        self.diagnostic_vm_creation_elapsed
    }

    /// Sample the live Lua allocator for optional diagnostics, without claiming peak or post-destruction memory.
    /// 为可选诊断采样存活 Lua 分配器，不声称峰值或销毁后的内存。
    /// Return bytes while this module exclusively owns the actual VM; the caller emits after this read returns.
    /// 在此模块独占实际 VM 时返回字节；调用方在此读取返回后发送。
    pub(crate) fn diagnostic_lua_heap_bytes(&self) -> usize {
        self.vm.lua.used_memory()
    }
    /// Report whether captured initialized exports still permit the sole explicit closing attempt.
    /// 报告已捕获的初始化导出是否仍允许唯一显式关闭尝试。
    pub(crate) fn can_finalize(&self) -> bool {
        !self.closed && !self.finalization_started && !self.exports.is_empty()
    }

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
        self.initialize_for_session(control, None)
    }

    /// Initialize under `control` with an optional trusted session identity; return success after capturing exports or an error.
    /// 在 `control` 下使用可选可信会话身份初始化；捕获导出后返回成功，否则返回错误。
    pub(crate) fn initialize_for_session(
        &mut self,
        control: Arc<CallControl>,
        session_id: Option<&str>,
    ) -> EmbeddedResult<()> {
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
        let source = self.prepared.definition().source.clone();
        // Resolve exact functions only after every value contract has compiled successfully.
        // 仅在全部值契约编译成功后解析精确函数。
        let initialization_id = match control.operation_id()? {
            Some(operation_id) => operation_id,
            None => self.initialization_id.clone(),
        };
        // Source evaluation measurement is fixed local metadata, never a callback from the Lua stack.
        // 源码求值测量是固定局部元数据，绝不是从 Lua 栈发起回调。
        let measure_bundle_evaluation = self.diagnostic_measurement_enabled;
        // No interval exists until the original source evaluation actually starts.
        // 原源码求值实际开始前不存在区间。
        let mut bundle_evaluation_elapsed = None;
        let exports = self.run(
            &context,
            control,
            &initialization_id,
            session_id,
            capabilities::CapabilityCallPhase::Initialization,
            |lua| {
                // The module return shape is fixed by the declared runtime protocol.
                // 模块返回形状由声明的运行时协议固定。
                // This interval contains both Lua compilation and actual source execution, including nested host work.
                // 此区间包含 Lua 编译及实际源码执行，包括嵌套宿主工作。
                let evaluation_started = measure_bundle_evaluation.then(std::time::Instant::now);
                // Preserve returned Lua errors while retaining only the interval actually entered.
                // 保留返回的 Lua 错误，同时仅保留实际进入的区间。
                let evaluated = lua
                    .load(&source)
                    .set_name("embedded_module")
                    .eval::<Table>();
                bundle_evaluation_elapsed = evaluation_started.map(|started| started.elapsed());
                let table = evaluated?;
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
            },
        );
        self.diagnostic_bundle_evaluation_elapsed = bundle_evaluation_elapsed;
        self.exports = exports?;
        Ok(())
    }

    /// Invoke the exact function and values in `invocation` without compiling Lua source.
    /// 使用 `invocation` 中的精确函数与值调用，不编译 Lua 源码。
    /// Return structured JSON; failed execution or cleanup prevents subsequent reuse.
    /// 返回结构化 JSON；执行或清理失败会阻止后续复用。
    pub fn invoke(&mut self, invocation: ModuleInvocation<'_>) -> EmbeddedResult<Value> {
        if self.closed || !self.reusable || self.finalization_started {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Closed,
                "module is closed or requires retirement",
            ));
        }
        self.invoke_export(invocation)
    }

    /// Attempt the declared closing export once in this initialized VM using the supplied finite budget.
    /// 使用提供的有限预算，在此已初始化 VM 中至多尝试一次声明的关闭导出。
    /// Return only the closing result; failures remain terminal and never authorize source replay or reuse.
    /// 只返回关闭结果；失败仍是终态，不授权重放源码或复用实例。
    pub(crate) fn finalize(&mut self, invocation: ModuleInvocation<'_>) -> EmbeddedResult<Value> {
        if !self.can_finalize() {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Closed,
                "module is not initialized or finalization was already attempted",
            ));
        }
        // Claim before validation and user code so every error and unwind remains at most once.
        // 在校验和用户代码前认领，确保所有错误和栈展开仍至多执行一次。
        self.finalization_started = true;
        self.reusable = false;
        // Preserve the caller-owned business outcome and use only the separate closing invocation.
        // 保留调用方拥有的业务结果，只使用独立的关闭调用。
        let result = self.invoke_export(invocation);
        self.reusable = false;
        result
    }

    /// Execute one captured export with its declared schemas, trusted context and explicit budget.
    /// 使用声明的 Schema、可信上下文和显式预算执行一个已捕获导出。
    /// Return the validated value or execution error; the caller owns lifecycle admission.
    /// 返回校验后的值或执行错误；调用方负责生命周期入场。
    fn invoke_export(&mut self, invocation: ModuleInvocation<'_>) -> EmbeddedResult<Value> {
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
        // Reject unsafe integer arguments before request setup or any business Lua can execute.
        // 在请求准备及任何业务 Lua 执行前拒绝不安全整数参数。
        LuaEngine::validate_embedded_json_value(invocation.arguments, "arguments")?;
        self.run(
            invocation.context,
            invocation.control,
            invocation.operation_id,
            invocation.session_id,
            capabilities::CapabilityCallPhase::Export,
            |lua| {
                // Use the same protected container identities as native capability conversion.
                // 使用与原生能力转换相同的受保护容器身份。
                let argument = values::to_lua(lua, invocation.arguments, "arguments")?;
                // Direct function calls do not compile a new wrapper for every request.
                // 直接函数调用不为每次请求编译新包装。
                let result = export.function.call::<LuaValue>(argument)?;
                // Output contract validation happens before request-owned resources commit.
                // 输出契约在请求所属资源提交前校验。
                let mut value = lua.from_value(result)?;
                // LuaJIT inferred integers outside the continuous exact domain are explicitly floating JSON.
                // LuaJIT 推断的连续精确域外整数明确编码为 JSON 浮点数。
                values::normalize_lua_json(&mut value);
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
        phase: capabilities::CapabilityCallPhase,
        execute: impl FnOnce(&Lua) -> mlua::Result<T>,
    ) -> EmbeddedResult<T> {
        // Each execution owns only its actual cleanup observation, never the prior reused request's scalar.
        // 每次执行仅拥有实际清理观测，绝不使用之前复用请求的标量。
        self.diagnostic_request_cleanup_elapsed = None;
        self.reusable = false;
        control.check()?;
        // Context projection also crosses the JSON-to-Lua boundary before module initialization or exports.
        // 上下文投影也在模块初始化或导出前穿过 JSON 到 Lua 边界。
        LuaEngine::validate_embedded_context(context)?;
        self.paths
            .validate_live_system_directory_identities()
            .map_err(execution_error)?;
        // An embedded module always owns this validated exact host package context.
        // 嵌入式模块始终拥有此已校验的宿主精确包上下文。
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
                    self.prepared.definition(),
                    operation_id.to_owned(),
                    session_id.map(str::to_owned),
                    control.request_id()?,
                )
            })
            .transpose()?;
        self.vm
            .lua
            .set_app_data(capabilities::CapabilityCallContext {
                caller,
                control: Arc::clone(&control),
                phase,
            });
        // Save the result before unconditional request-context cleanup.
        // 在无条件清理请求上下文前保存执行结果。
        let result = execute(&self.vm.lua);
        drop(guard);
        // Measure only the real post-execution reset call, not automatic GC or transaction publication.
        // 仅测量真实执行后重置调用，不测量自动 GC 或事务发布。
        let cleanup_started = self
            .diagnostic_measurement_enabled
            .then(std::time::Instant::now);
        // Preserve the original cleanup result before any later budget or transaction decisions.
        // 在后续任何预算或事务决策前保留原清理结果。
        let cleanup =
            reset_pooled_vm_request_scope(&self.vm.lua, self.engine.host_options.as_ref());
        self.diagnostic_request_cleanup_elapsed = cleanup_started.map(|started| started.elapsed());
        cleanup.map_err(execution_error)?;
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
        self.prepared.definition()
    }

    /// Report whether successful execution and cleanup permit another call.
    /// 报告执行及清理成功后是否允许再次调用。
    pub fn is_reusable(&self) -> bool {
        self.reusable && !self.closed && !self.finalization_started
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
