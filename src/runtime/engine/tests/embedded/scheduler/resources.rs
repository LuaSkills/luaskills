//! Real native ownership through the public scheduler, fixed sessions, and generation replacement.
//! 贯穿公开调度器、固定会话及代次替换的真实原生所有权。

use super::sessions::{closed_session, opened, session_call};
use super::*;
use std::fs::{File, OpenOptions};

/// One observer and operation budget for this resource-ownership scenario.
/// 此资源所有权场景统一的观察与操作预算。
const OWNER_WAIT: Duration = Duration::from_secs(3);

/// Opens a real shared file lease at `path` and returns its owner plus a separate collection probe.
/// 在 `path` 打开真实共享文件租约，返回所有者及独立回收探针。
fn generation_file(path: &std::path::Path) -> (Arc<File>, File) {
    // Each generation is protected by a different OS lock, so a replacement cannot mask early release.
    // 每个代次由不同操作系统锁保护，因此替换代次不能掩盖提前释放。
    let file = Arc::new(
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .expect("create generation file"),
    );
    file.try_lock_shared().expect("hold real generation lease");
    // A collector uses an independent handle rather than the locking owner's duplicate.
    // 回收者使用独立句柄，而非锁所有者的复制句柄。
    let collector = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open collection probe");
    (file, collector)
}

/// Proves old fixed sessions retain only their real generation and historical metadata retains neither.
/// 证明旧固定会话只保留其真实代次，而历史元数据不保留任何代次。
#[test]
fn embedded_scheduler_host_owner_tracks_real_sessions_across_replacement() {
    // Both generations use the same trusted fixture but independent physical resource owners.
    // 两个代次使用同一可信夹具，但采用独立物理资源所有者。
    let layout = SystemRuntimeTestLayout::new("embedded scheduler generation owners");
    // Exercise the public runtime rather than bypassing its admission and session ownership.
    // 使用公开运行时，而不绕过其入场及会话所有权。
    let runtime = runtime(&layout, pool_config());
    // The only strong old-generation owner is transferred into the registered pool.
    // 旧代次唯一强所有者转入注册池。
    let (old_file, old_collector) =
        generation_file(&layout.package_root.join("old-generation.lease"));
    // Retaining this observer must not itself prevent collection.
    // 保留此观察者本身不得阻止回收。
    let old_lifetime = Arc::downgrade(&old_file);
    // A fixed session genuinely keeps the original VM alive after its call completes.
    // 固定会话在调用完成后真实保留原始 VM。
    let old_pool = runtime
        .register_pool_with_owner(
            definition(&layout, "return {call=function() return 'old' end}"),
            pool_policy(InstanceReuse::Session),
            permissions(),
            "old-revision".into(),
            ModuleResourceOwner::new(old_file),
        )
        .expect("register old owner");
    // Session creation initializes the actual Lua module once.
    // 会话创建实际初始化一次 Lua 模块。
    let session = opened(&runtime, &old_pool);
    // Retain a completed operation handle throughout retirement to check metadata-only ownership.
    // 在退役期间保留已完成操作句柄，检查仅含元数据的所有权。
    let old_operation = session_call(&runtime, &session, Value::Null);
    assert_eq!(
        old_operation
            .wait(OWNER_WAIT)
            .expect("old call completes")
            .value,
        Some(json!("old"))
    );
    // A separate generation gets a distinct real resource owner before routing changes.
    // 路由改变之前，独立代次取得不同真实资源所有者。
    let (new_file, new_collector) =
        generation_file(&layout.package_root.join("new-generation.lease"));
    // Definition identity is explicit and cannot be inferred from the opaque resource owner.
    // 定义身份显式提供，不能由不透明资源所有者推断。
    let mut replacement = definition(&layout, "return {call=function() return 'new' end}");
    replacement.generation = "generation-two".into();
    // Unused registered pools still own source resources required by future initialization.
    // 尚未使用的注册池仍拥有未来初始化所需源码资源。
    let new_pool = runtime
        .register_pool_with_owner(
            replacement,
            pool_policy(InstanceReuse::Reusable),
            permissions(),
            "new-revision".into(),
            ModuleResourceOwner::new(new_file),
        )
        .expect("register replacement owner");
    runtime
        .close_pool(&old_pool)
        .expect("close old generation admission");
    assert!(old_lifetime.upgrade().is_some());
    assert!(matches!(
        old_collector.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    assert!(matches!(
        new_collector.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    runtime
        .close_session(&session)
        .expect("retire original fixed VM");
    closed_session(&runtime, &session);
    assert!(
        old_lifetime.upgrade().is_none(),
        "retained pool, session, and operation metadata cannot pin a retired generation"
    );
    old_collector
        .try_lock()
        .expect("old generation is collectible after actual retirement");
    assert!(matches!(
        new_collector.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    assert_eq!(
        old_operation
            .snapshot()
            .expect("retained historical operation")
            .value,
        Some(json!("old"))
    );
    assert_eq!(
        runtime
            .submit(call(&new_pool, Value::Null), OWNER_WAIT)
            .expect("replacement admission")
            .wait(OWNER_WAIT)
            .expect("replacement completes")
            .value,
        Some(json!("new"))
    );
    shutdown(&runtime);
    new_collector
        .try_lock()
        .expect("closed runtime releases the final real generation owner");
    assert_eq!(
        runtime
            .session(&session)
            .expect("retained closed session record")
            .phase,
        EmbeddedSessionPhase::Closed
    );
}
