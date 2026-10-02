//! Prepared contracts preserve real admission rejection and independent VM validation.
//! 已准备契约保持真实入场拒绝及独立 VM 校验。

use super::*;

/// Close manager and wait for its actual retirement worker to exit within a failure deadline.
/// 关闭 manager，并在失败截止时间内等待其实际退役工作线程退出。
/// Return unit only after original physical ownership and worker shutdown are confirmed.
/// 仅原物理所有权及工作线程关闭已确认后返回空值。
fn close_manager(manager: &Arc<EmbeddedPoolManager>) {
    manager.request_close().unwrap();
    // This deadline detects shutdown failure without assuming a particular worker scheduling delay.
    // 此截止时间检测关闭失败，不假定特定工作线程调度延迟。
    let deadline = Instant::now() + Duration::from_secs(5);
    while !manager.poll_closed().unwrap() {
        assert!(
            Instant::now() < deadline,
            "original manager must physically close"
        );
        std::thread::yield_now();
    }
}

/// Reject bad schema or closing declarations through each existing public entry before Lua initialization.
/// 通过每个既有公开入口在 Lua 初始化前拒绝错误 Schema 或关闭声明。
/// No parameters or return value; a real file witness and unchanged capacity expose premature execution.
/// 无参数或返回值；真实文件见证及未变容量揭示过早执行。
#[test]
fn embedded_prepared_contracts_reject_invalid_registration_without_execution() {
    // The existing package fixture permits the initialization file write used as a real witness.
    // 既有包夹具允许作为真实见证的初始化文件写入。
    let layout = SystemRuntimeTestLayout::new("embedded prepared admission");
    // Direct and governed entry points share the same real engine and filesystem boundary.
    // 直接及受治理入口共享相同真实引擎及文件系统边界。
    let engine = Arc::new(make_runtime_test_engine_with_host_options(
        layout.host_options(),
    ));
    // Initialization writes before returning any functions, so an executed invalid declaration is observable.
    // 初始化在返回任何函数前写入，因此执行无效声明可被观察。
    let mut original = definition(
        &layout,
        "vulcan.fs.write('prepared-started','started'); return {call=function(a) return a end}",
    );
    original.exports[0].input_schema = json!({"type":"integer"});
    original.exports[0].output_schema = json!({"type":"integer"});
    original.finalizer = Some(ModuleFinalizer {
        export: "call".into(),
        arguments: json!(1),
        timeout_ms: 1000,
    });
    // Prove the source and witness are operational before testing rejection at preparation.
    // 在验证准备阶段拒绝前，证明源码及见证实际可运行。
    let mut baseline = engine
        .create_embedded_module(original.clone(), "prepared-baseline", control())
        .unwrap();
    assert!(layout.package_root.join("prepared-started").is_file());
    baseline.close().unwrap();
    fs::remove_file(layout.package_root.join("prepared-started")).unwrap();
    // Low-level public pool registration must validate just as strictly as the formal scheduler.
    // 低层公开池注册必须与正式调度器一样严格校验。
    let manager = EmbeddedPoolManager::new(Arc::clone(&engine), pool_config()).unwrap();
    // Existing runtime fixture supplies real plugin, capability, worker and shutdown authority.
    // 既有运行时夹具提供真实插件、能力、工作线程及关闭权威。
    let runtime = runtime(&layout, pool_config());
    // Each malformed declaration targets a distinct required guard rather than checking prepared fields.
    // 每个错误声明针对一个不同必要护栏，而非检查已准备字段。
    for case in [
        "input",
        "output",
        "closing-input",
        "closing-timeout",
        "closing-value",
        "closing-export",
        "duplicate",
    ] {
        // Only the tested contract changes; valid paths and working source retain the actual execution witness.
        // 仅改变被测契约；有效路径及可运行源码保留实际执行见证。
        let mut invalid = original.clone();
        match case {
            "input" => invalid.exports[0].input_schema = json!({"type":"invalid"}),
            "output" => invalid.exports[0].output_schema = json!({"type":"invalid"}),
            "closing-input" => invalid.finalizer.as_mut().unwrap().arguments = json!("wrong"),
            "closing-timeout" => invalid.finalizer.as_mut().unwrap().timeout_ms = 0,
            "closing-value" => {
                invalid.exports[0].input_schema = json!(true);
                invalid.finalizer.as_mut().unwrap().arguments = json!(u64::MAX);
            }
            "closing-export" => invalid.finalizer.as_mut().unwrap().export = "undeclared".into(),
            "duplicate" => invalid.exports.push(invalid.exports[0].clone()),
            _ => unreachable!(),
        }
        assert_eq!(
            engine
                .create_embedded_module(invalid.clone(), case, control())
                .err()
                .expect("direct entry must reject invalid preparation")
                .code,
            EmbeddedErrorCode::InvalidArgument,
            "{case}"
        );
        assert_eq!(
            manager
                .create_pool(
                    case.into(),
                    invalid.clone(),
                    pool_policy(InstanceReuse::SingleCall)
                )
                .err()
                .expect("public pool entry must reject invalid preparation")
                .code,
            EmbeddedErrorCode::InvalidArgument,
            "{case}"
        );
        assert_eq!(
            runtime
                .register_pool(
                    invalid,
                    pool_policy(InstanceReuse::SingleCall),
                    permissions(),
                    case.into()
                )
                .expect_err("scheduler must reject invalid preparation")
                .code,
            EmbeddedErrorCode::InvalidArgument,
            "{case}"
        );
        assert!(
            !layout.package_root.join("prepared-started").exists(),
            "{case} executed initialization"
        );
        assert_eq!(manager.usage().unwrap().resident, 0);
    }
    shutdown(&runtime);
    close_manager(&manager);
}

/// Share one pool's prepared contracts across fresh SingleCall VMs without sharing Lua state or skipping validation.
/// 跨新 SingleCall VM 共享单个池的已准备契约，不共享 Lua 状态或跳过校验。
/// No parameters or return value; original filesystem effects distinguish rejected inputs from rejected outputs.
/// 无参数或返回值；原文件系统副作用区分输入拒绝与输出拒绝。
#[test]
fn embedded_prepared_contracts_validate_each_fresh_single_call_vm() {
    // Real immutable package paths are reused while every lease receives a distinct actual VM.
    // 复用真实不可变包路径，同时每个租借获得不同实际 VM。
    let layout = SystemRuntimeTestLayout::new("embedded prepared fresh vms");
    // Existing low-level manager enforces real permits and physical retirement ownership.
    // 既有低层管理器执行真实许可及物理退役所有权。
    let manager = EmbeddedPoolManager::new(
        Arc::new(make_runtime_test_engine_with_host_options(
            layout.host_options(),
        )),
        pool_config(),
    )
    .unwrap();
    // Per-VM state must restart at one, and the output-only failure must occur after its actual file write.
    // 逐 VM 状态必须从一重新开始，仅输出失败必须发生在其实际文件写入之后。
    let mut declaration = definition(
        &layout,
        "local n=0; return {call=function(a) n=n+1; vulcan.fs.write('prepared-effect-'..a,tostring(n)); if a==2 then return 'wrong' end; return n end}",
    );
    declaration.exports[0].input_schema = json!({"type":"integer","minimum":1});
    declaration.exports[0].output_schema = json!({"type":"integer"});
    // This single registered pool owns the original definition and compiled contracts throughout every checkout.
    // 此单个已注册池在每次借用期间拥有原定义及已编译契约。
    let pool = manager
        .create_pool(
            "prepared-single".into(),
            declaration,
            pool_policy(InstanceReuse::SingleCall),
        )
        .unwrap();
    // Actual instance identities verify fresh allocation, rather than assuming it from the declared policy.
    // 实际实例身份验证新分配，而非根据声明策略假定。
    let mut instances = BTreeSet::new();
    // Invalid input precedes business; invalid output follows business; later success proves the same pool remains usable.
    // 无效输入先于业务；无效输出后于业务；后续成功证明相同池仍可使用。
    for argument in [-1, 1, 2, 1] {
        // Each checkout constructs and initializes an actual independent module using the pool's retained contracts.
        // 每次借用使用池保留契约构造并初始化实际独立模块。
        let mut lease = pool.acquire(control()).unwrap();
        assert!(instances.insert(lease.instance_id().unwrap().to_owned()));
        // Invoke the actual function with its original schema and instance-specific request context.
        // 使用原 Schema 及实例特定请求上下文调用实际函数。
        let result = lease.invoke(ModuleInvocation {
            operation_id: "prepared-contract-business",
            session_id: None,
            export: "call",
            arguments: &json!(argument),
            context: &LuaInvocationContext::default(),
            control: control(),
        });
        if argument == 1 {
            assert_eq!(result.unwrap(), json!(1));
            assert_eq!(
                fs::read_to_string(layout.package_root.join("prepared-effect-1")).unwrap(),
                "1"
            );
        } else {
            assert!(result.is_err());
            if argument == -1 {
                assert!(!layout.package_root.join("prepared-effect--1").exists());
            } else {
                assert_eq!(
                    fs::read_to_string(layout.package_root.join("prepared-effect-2")).unwrap(),
                    "1"
                );
            }
        }
        // Explicit release retains original evidence until the actual module and capacity have retired.
        // 明确释放保留原证据，直到实际模块及容量已退役。
        let ModuleRelease::Retiring(receipt) = lease.finish().unwrap() else {
            panic!("SingleCall must physically retire its original VM");
        };
        assert_eq!(
            receipt.wait(Duration::from_secs(5)).unwrap().phase,
            ModuleRetirementPhase::Completed
        );
        super::super::pools::drained(&pool);
    }
    pool.close().unwrap();
    close_manager(&manager);
}
