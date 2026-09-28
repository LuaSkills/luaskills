//! Real host file ownership across pool admission, Lua finalization, and generation retirement.
//! 跨池入场、Lua 终结及代次退役的真实宿主文件所有权。

use super::*;
use crate::runtime::embedded::{ModuleRelease, ModuleResourceOwner, ModuleRetirementPhase};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

/// Maximum observer wait; the Lua watchdog derives its longer fallback from the same value.
/// 观察等待上限；Lua 看门狗从同一值派生较长的兜底时限。
const OWNER_WAIT: Duration = Duration::from_secs(5);

/// Releases the actual Lua finalizer barrier even when a test assertion unwinds.
/// 即使测试断言栈展开，也释放实际 Lua 终结器屏障。
struct ReleaseFinalizer(PathBuf);

impl Drop for ReleaseFinalizer {
    /// Writes the release marker without introducing a second panic on an already failing test.
    /// 写入释放标记，不在已经失败的测试中引入第二次 panic。
    fn drop(&mut self) {
        let _ = std::fs::write(&self.0, b"release");
    }
}

/// Creates a shared lease file at `path`, returning both its real owner and an independent collection probe.
/// 在 `path` 创建共享租约文件，返回真实所有者及独立回收探针。
fn locked_resource(path: &Path) -> (Arc<File>, File) {
    // No plugin code supplies this ownership object or controls its destructor.
    // 插件代码不提供此所有权对象，也不控制其析构器。
    let owner = Arc::new(
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .expect("create host generation lease"),
    );
    owner
        .try_lock_shared()
        .expect("acquire actual shared host generation lease");
    // Collection probes a distinct handle, matching the immutable package store's lock protocol.
    // 回收探测独立句柄，匹配不可变插件包存储的锁协议。
    let probe = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open independent collection probe");
    (owner, probe)
}

/// Returns only after `path` exists or fails at the shared observer deadline.
/// 仅在 `path` 出现后返回，否则在共享观察截止时间失败。
fn wait_for_file(path: &Path) {
    // The marker comes from actual VM destruction, not aggregate accounting or a timing assumption.
    // 标记来自实际 VM 销毁，而非聚合记账或计时假设。
    let deadline = Instant::now() + OWNER_WAIT;
    while !path.exists() && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(path.exists(), "actual Lua finalizer must reach its marker");
}

/// Actual package ownership survives both successful loading and rejected initialization until VM destruction returns.
/// 实际插件包所有权在成功加载及初始化拒绝两种情况下均保留到 VM 销毁返回。
#[test]
fn embedded_pool_host_owner_waits_for_real_finalizer_and_failed_initialization() {
    for fail_initialization in [false, true] {
        // Each scenario owns a real Lua VM and independent filesystem synchronization.
        // 每种场景都拥有真实 Lua VM 及独立文件系统同步。
        let layout = SystemRuntimeTestLayout::new("embedded generation host owner");
        // One existing retirement service performs actual cleanup and Lua destruction.
        // 一个既有退役服务执行实际清理及 Lua 销毁。
        let manager = pool_manager(&layout);
        // This guard prevents a failed test from stranding the finalizer.
        // 此保护对象防止失败测试遗留终结器。
        let release = ReleaseFinalizer(layout.package_root.join("release-host-finalizer"));
        // The resource is a real OS lease, not a boolean pretending that a package remains pinned.
        // 资源是真实操作系统租约，而非假装插件包仍固定的布尔值。
        let (file, collector) = locked_resource(&layout.package_root.join("host-generation.lease"));
        // Observation is weak and must never prolong the tested owner's lifetime.
        // 观察使用弱引用，绝不延长被测所有者的寿命。
        let lifetime = Arc::downgrade(&file);
        // The finalizer retains captured IO through Lua destruction and signals both sides of its barrier.
        // 终结器在 Lua 销毁期间保留已捕获 IO，并标记屏障两端。
        let source = format!(
            r#"
            local open, clock = io.open, os.clock
            local finalizer = newproxy(true)
            getmetatable(finalizer).__gc = function()
                local entered = assert(open('entered-host-finalizer', 'w'))
                entered:write('entered'); entered:close()
                local deadline = clock() + {}
                repeat
                    local ok, marker = pcall(open, 'release-host-finalizer', 'r')
                    if ok and marker then marker:close(); break end
                until clock() >= deadline
                local finished = assert(open('finished-host-finalizer', 'w'))
                finished:write('finished'); finished:close()
            end
            {}
            return {{call=function() return finalizer ~= nil end}}
        "#,
            OWNER_WAIT.as_secs() * 2,
            if fail_initialization {
                "error('reject source initialization')"
            } else {
                ""
            }
        );
        // Transfer the caller's only strong file owner into the actual module registration.
        // 把调用方唯一文件强所有者转入实际模块注册。
        let pool = manager
            .create_pool_with_owner(
                "owned-generation".into(),
                definition(&layout, &source),
                pool_policy(InstanceReuse::SingleCall),
                None,
                ModuleResourceOwner::new(file),
            )
            .expect("register generation owner");
        // Failed initialization and successful single calls both expose the same exact retirement authority.
        // 失败初始化及成功单次调用都暴露相同精确退役权威。
        let receipt = if fail_initialization {
            match pool.acquire_tracked(control()) {
                Err(failure) => failure
                    .retirement
                    .expect("failed initialization owns an actual VM"),
                Ok(_) => panic!("source initialization must fail"),
            }
        } else {
            match pool
                .acquire_tracked(control())
                .expect("load owned generation")
                .finish()
                .expect("retire owned generation")
            {
                ModuleRelease::Retiring(receipt) => receipt,
                _ => panic!("single-call module must retire"),
            }
        };
        pool.close()
            .expect("close admission during real retirement");
        wait_for_file(&layout.package_root.join("entered-host-finalizer"));
        assert!(lifetime.upgrade().is_some());
        assert!(matches!(
            collector.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        assert!(!layout.package_root.join("finished-host-finalizer").exists());
        assert_ne!(
            receipt
                .wait(Duration::ZERO)
                .expect("read actual pending retirement")
                .phase,
            ModuleRetirementPhase::Completed
        );
        drop(release);
        assert_eq!(
            receipt
                .wait(OWNER_WAIT)
                .expect("wait for actual finalizer completion")
                .phase,
            ModuleRetirementPhase::Completed
        );
        assert!(layout.package_root.join("finished-host-finalizer").exists());
        assert!(
            lifetime.upgrade().is_none(),
            "closed pool handles and receipts must not retain physical package ownership"
        );
        collector
            .try_lock()
            .expect("collect only after actual VM and host owner exit");
        assert_eq!(
            pool.usage().expect("read retained closed handle").resident,
            0
        );
    }
}

/// Shared owners cover pending reservations and idle reuse but release without dropping closed pool handles.
/// 共享所有者覆盖等待预留及空闲复用，但无需丢弃已关闭池句柄即可释放。
#[test]
fn embedded_pool_host_owner_spans_pending_and_multiple_pool_lifetimes() {
    // Two real pools represent independent execution domains of the same immutable host generation.
    // 两个真实池表示同一不可变宿主代次的独立执行域。
    let layout = SystemRuntimeTestLayout::new("embedded shared generation owner");
    // Parent accounting is shared exactly as in normal mixed-pool operation.
    // 父级记账与正常混合池操作相同地共享。
    let manager = pool_manager(&layout);
    // Independent collector cannot acquire its lock until both pool lifetimes finish.
    // 独立回收者只有在两个池生命周期都结束后才能取得锁。
    let (file, collector) = locked_resource(&layout.package_root.join("shared-generation.lease"));
    // Clone the actual ownership wrapper across pools, not the package path.
    // 跨池克隆实际所有权包装，而非插件包路径。
    let owner = ModuleResourceOwner::new(file);
    // The first pool reserves capacity without yet creating Lua state.
    // 第一个池预留容量，但尚未创建 Lua 状态。
    let first = manager
        .create_pool_with_owner(
            "pending-owner".into(),
            definition(&layout, "return {call=function() return true end}"),
            pool_policy(InstanceReuse::Reusable),
            None,
            owner.clone(),
        )
        .expect("register pending pool owner");
    // The second pool holds a genuine reusable VM under the same host resource owner.
    // 第二个池在同一宿主资源所有者下持有真正的可复用 VM。
    let second = manager
        .create_pool_with_owner(
            "idle-owner".into(),
            definition(&layout, "return {call=function() return true end}"),
            pool_policy(InstanceReuse::Reusable),
            None,
            owner,
        )
        .expect("register idle pool owner");
    // Pending leases retain their registration even after admission closes.
    // 即使入场关闭，等待租约仍保留其注册。
    let pending = first
        .prepare(&control())
        .expect("reserve before VM allocation");
    // Actual initialization gives the second pool a resident that can later become idle.
    // 实际初始化使第二个池获得随后可变为空闲的常驻实例。
    let lease = second
        .acquire_tracked(control())
        .expect("allocate reusable owned VM");
    // The receipt observes actual final destruction without retaining the host resource.
    // 回执观察实际最终销毁，不保留宿主资源。
    let receipt = lease
        .retirement_handle()
        .expect("capture exact retirement receipt");
    assert!(matches!(
        lease.finish().expect("return real VM to pool"),
        ModuleRelease::ReturnedToPool
    ));
    first.close().expect("close pool with pending allocation");
    assert!(matches!(
        collector.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    second.close().expect("retire idle owned VM");
    assert_eq!(
        receipt
            .wait(OWNER_WAIT)
            .expect("wait for last owned VM")
            .phase,
        ModuleRetirementPhase::Completed
    );
    // Only the not-yet-initialized reservation remains; another pool cannot mask premature release.
    // 现在只剩尚未初始化的预留；另一个池不能掩盖提前释放。
    assert!(matches!(
        collector.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    assert!(matches!(
        pending.finish().expect("release pending reservation"),
        ModuleRelease::NoInstance
    ));
    collector
        .try_lock()
        .expect("release shared owner after both actual pool lifetimes");
    assert_eq!(first.usage().expect("read first closed pool").resident, 0);
    assert_eq!(second.usage().expect("read second closed pool").resident, 0);
}
