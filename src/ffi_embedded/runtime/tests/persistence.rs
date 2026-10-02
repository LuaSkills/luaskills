//! Durable FFI ownership tests use real SQLite gates, native requests and retained writer receipts.
//! 持久 FFI 所有权测试使用真实 SQLite 门禁、原生请求和保留的写入回执。

use super::*;
use crate::ffi_embedded::tests::runtimes::{command, reserve, transport};
use crate::runtime::embedded::{EffectState, OperationContext, OperationPhase, OperationSnapshot};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::mpsc;

/// A private directory outliving every database handle created by its test.
/// 比测试创建的每个数据库句柄存活更久的私有目录。
struct Directory(PathBuf);

impl Directory {
    /// Create one unique directory and return its exact cleanup owner.
    /// 创建唯一目录，返回其精确清理所有者。
    fn new() -> Self {
        // Random bytes avoid collisions with other processes and stale failed-test directories.
        // 随机字节避免与其他进程及失败测试遗留目录冲突。
        let mut entropy = [0_u8; 16];
        getrandom::fill(&mut entropy).unwrap();
        // Hex names remain valid on each supported filesystem.
        // 十六进制名称在每种支持的文件系统上均有效。
        let name = entropy
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        // Only this newly created directory is eligible for fixture cleanup.
        // 仅此新创建目录可由夹具清理。
        let path = std::env::temp_dir().join(format!("luaskills-ffi-history-{name}"));
        std::fs::create_dir(&path).unwrap();
        // Retain the sole cleanup owner before resolving symlinked temporary parents for SQLite NOFOLLOW.
        // 在为 SQLite NOFOLLOW 解析含符号链接的临时父目录前，保留唯一清理所有者。
        let mut owner = Self(path);
        owner.0 = std::fs::canonicalize(&owner.0).unwrap();
        owner
    }

    /// Return complete explicit wire budgets and a host-owned absolute database path.
    /// 返回完整显式线协议预算与宿主拥有的绝对数据库路径。
    fn config(&self) -> Value {
        json!({"path":self.0.join("operations.db"),
            "journal":{"max_records":16,"max_record_bytes":16384,"max_database_bytes":131072},
            "worker":{"max_pending_writes":8,"max_pending_bytes":65536}})
    }

    /// Open this directory's real writer for a unit-level lifetime fixture.
    /// 为单元寿命夹具打开此目录的真实写入者。
    fn owner(&self) -> Arc<PersistenceOwner> {
        PersistenceOwner::new(serde_json::from_value(self.config()).unwrap()).unwrap()
    }
}

impl Drop for Directory {
    /// Remove only this test-owned directory after all journal handles have closed.
    /// 全部日志句柄关闭后，仅移除此测试拥有的目录。
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Return a terminal no-effect record suitable for actual writer ownership and deletion tests.
/// 返回适于真实写入者所有权及删除测试的无副作用终态记录。
fn completed() -> Arc<OperationSnapshot> {
    Arc::new(OperationSnapshot {
        finalization: None,
        context: OperationContext::Unbound,
        operation_id: "historical-operation".into(),
        phase: OperationPhase::Succeeded,
        cancellation_requested: false,
        effects: EffectState::NotStarted,
        value: Some(json!({"original":true})),
        error: None,
        host_effects: Vec::new(),
    })
}

/// Initialize the exact native slot with optional storage and return its retained ownership.
/// 以可选存储初始化精确原生槽，返回其保留所有权。
fn native(id: u64, persistence: Value) -> Arc<RuntimeSlot> {
    // Reservation gives the host a stable identity before any native resources exist.
    // 预留在任何原生资源存在前为宿主提供稳定身份。
    let runtime_id = reserve(id);
    // The native success receipt acknowledges the attempt; snapshot owns the actual outcome.
    // 原生成功回执确认尝试；实际结果由快照拥有。
    let result = command(
        id,
        json!({"type":"runtime_initialize","runtime_id":runtime_id,
        "engine_options":engine_options(),"runtime_config":runtime_config(),"persistence":persistence}),
    );
    assert_eq!(result["status"], "ok", "{result}");
    crate::ffi_embedded::transport::get(id)
        .unwrap()
        .runtime(&runtime_id)
        .unwrap()
}

/// Send one nested command through the public ABI using this exact slot identity.
/// 使用此精确槽身份，通过公开 ABI 发送一个嵌套命令。
fn run(id: u64, slot: &RuntimeSlot, operation: Value) -> Value {
    command(
        id,
        json!({"type":"runtime","runtime_id":slot.id,"operation":operation}),
    )
}

/// Drain and free every native registration after the test has released its borrowed storage owners.
/// 测试释放借用的存储所有者后，排空并释放全部原生注册。
fn finish_native(id: u64, slot: Arc<RuntimeSlot>) {
    slot.request_close().unwrap();
    wait_closed(&slot);
    assert_eq!(
        command(id, json!({"type":"runtime_free","runtime_id":slot.id}))["status"],
        "ok"
    );
    drop(slot);
    assert_eq!(
        crate::ffi_embedded::luaskills_ffi_embedded_transport_close_v1(id),
        0
    );
    assert_eq!(
        crate::ffi_embedded::luaskills_ffi_embedded_transport_free_v1(id),
        0
    );
}

/// Failed construction retains its writer, including close during initialization and completed receipt ownership.
/// 构造失败保留其写入者，包含初始化中关闭及已完成回执所有权。
#[test]
fn ffi_embedded_persistence_failed_initialization_retains_writer_and_receipt() {
    // Directory ownership encloses both constructor and writer lifetimes.
    // 目录所有权包围构造器与写入者寿命。
    let directory = Directory::new();
    // The exact storage owner is published before the injected later constructor failure.
    // 在注入的后续构造失败前发布精确存储所有者。
    let owner = directory.owner();
    // A completed receipt must continue to prevent safe unload even after the writer exits.
    // 即使写入者退出，已完成回执也必须继续阻止安全卸载。
    let receipt = owner
        .writer
        .submit("original-runtime", None, completed())
        .unwrap();
    assert!(
        receipt
            .wait(Duration::from_secs(5))
            .unwrap()
            .error
            .is_none()
    );
    // The slot is the authoritative owner after initialization fails.
    // 初始化失败后，槽是权威所有者。
    let slot = RuntimeSlot::new().unwrap();
    // This barrier proves close observes an unfinished factory rather than guessing its timing.
    // 此屏障证明关闭观测到未完成工厂，而不是猜测时序。
    let barrier = Arc::new(Barrier::new(2));
    // Constructor references are moved into the actual native initialization thread.
    // 构造器引用移入实际原生初始化线程。
    let creator_slot = Arc::clone(&slot);
    // The initializing thread retains the same barrier authority.
    // 初始化线程保留相同屏障权威。
    let creator_barrier = Arc::clone(&barrier);
    // A real worker already exists when the factory deliberately reports its later error.
    // 工厂故意报告后续错误时，真实工作线程已经存在。
    let creator = std::thread::spawn(move || {
        creator_slot.initialize_with(|| {
            creator_slot.lock()?.persistence = Some(owner);
            creator_barrier.wait();
            creator_barrier.wait();
            Err(internal(
                "injected failure after storage ownership publication",
            ))
        })
    });
    barrier.wait();
    slot.request_close().unwrap();
    assert!(!slot.snapshot().unwrap().closed);
    assert_eq!(slot.release().unwrap_err().code, EmbeddedErrorCode::Busy);
    barrier.wait();
    creator.join().unwrap().unwrap();
    // Failure can be observed while actual receipt ownership still keeps closure false.
    // 实际回执所有权仍使关闭为假时，失败可以被观测。
    let status = slot.snapshot().unwrap();
    assert_eq!(status.initialization, InitializationPhase::Failed);
    assert_eq!(status.persistence.unwrap().pending_writes, 1);
    assert!(!status.closed);
    assert_eq!(slot.release().unwrap_err().code, EmbeddedErrorCode::Busy);
    drop(receipt);
    wait_closed(&slot);
    slot.release().unwrap();
}

/// Invalid worker budgets fail before database creation; memory-only runtimes refuse storage commands explicitly.
/// 无效工作线程预算在创建数据库前失败；纯内存运行时明确拒绝存储命令。
#[test]
fn ffi_embedded_persistence_configuration_and_memory_mode_are_explicit() {
    // Invalid declarations must leave the exact host path absent.
    // 无效声明必须使精确宿主路径保持不存在。
    let directory = Directory::new();
    // Exercise decoding, initialization outcome and cleanup through the real ABI.
    // 通过真实 ABI 验证解码、初始化结果及清理。
    let id = transport();
    // Zero pending-write capacity is invalid, not an instruction to use memory mode.
    // 零待写容量无效，不是使用内存模式的指令。
    let mut config = directory.config();
    config["worker"]["max_pending_writes"] = json!(0);
    // The failed slot remains queryable and removable through ordinary lifetime rules.
    // 失败槽仍可按普通寿命规则查询及移除。
    let failed = native(id, config);
    assert_eq!(
        failed.snapshot().unwrap().initialization,
        InitializationPhase::Failed
    );
    assert_eq!(
        failed.snapshot().unwrap().error.unwrap().code,
        EmbeddedErrorCode::InvalidArgument
    );
    assert!(!directory.0.join("operations.db").exists());
    finish_native(id, failed);
    // A separate transport prevents unrelated reservation limits from affecting this check.
    // 独立传输防止无关预留上限影响此检查。
    let memory_id = transport();
    // Null is the declared optional absence, preserving existing initialization behavior.
    // 空值是已声明的可选缺省，保留既有初始化行为。
    let memory = native(memory_id, Value::Null);
    assert_eq!(
        memory.snapshot().unwrap().initialization,
        InitializationPhase::Ready
    );
    assert!(memory.snapshot().unwrap().persistence.is_none());
    for operation in [
        json!({"type":"storage_status"}),
        json!({"type":"storage_recover"}),
        json!({"type":"storage_worker_recover"}),
        json!({"type":"history_next"}),
        json!({"type":"history_get","history_runtime_id":"old","operation_id":"old"}),
        json!({"type":"history_forget","history_runtime_id":"old","operation_id":"old","expected_revision":1}),
    ] {
        assert_eq!(
            run(memory_id, &memory, operation)["error"]["code"],
            "unsupported"
        );
    }
    finish_native(memory_id, memory);
}

/// Blocked real history I/O keeps control available and prevents runtime release until its actual native call ends.
/// 阻塞的真实历史读写保持控制可用，并在实际原生调用结束前阻止运行时释放。
#[test]
fn ffi_embedded_persistence_disk_wait_preserves_control_and_native_lease() {
    // Retain the filesystem root outside all threads and connections.
    // 在所有线程及连接外保留文件系统根目录。
    let directory = Directory::new();
    // The real transport supports one blocked work request and one independent control request.
    // 真实传输支持一个阻塞工作请求和一个独立控制请求。
    let id = transport();
    // Ready storage is created through the same public initialization used by SDKs.
    // 就绪存储通过 SDK 使用的同一公开初始化创建。
    let slot = native(id, directory.config());
    // This test-only retained reference lets us hold the exact SQLite gate.
    // 此仅测试保留引用允许持有精确 SQLite 门禁。
    let owner = Arc::clone(slot.lock().unwrap().persistence.as_ref().unwrap());
    // Block the same lock acquired by history_get, without touching runtime metadata locks.
    // 阻塞 history_get 获取的同一锁，不接触运行时元数据锁。
    let gate = owner.journal.block_for_test();
    // Only the public request thread retains this slot reference.
    // 仅公开请求线程保留此槽引用。
    let read_slot = Arc::clone(&slot);
    // Completion observation proves the read cannot return while its real disk gate is held.
    // 完成观测证明实际磁盘门禁被持有时读取不能返回。
    let (sender, receiver) = mpsc::channel();
    // The work call owns its native lease until it returns and frees its response.
    // 工作调用直到返回并释放响应才释放其原生租借。
    let reader = std::thread::spawn(move || {
        sender
            .send(run(
                id,
                &read_slot,
                json!({"type":"history_get","history_runtime_id":"old","operation_id":"old"}),
            ))
            .unwrap()
    });
    // Wait for actual native acquisition, independent of thread scheduling speed.
    // 等待实际原生获取，不依赖线程调度速度。
    let deadline = Instant::now() + Duration::from_secs(5);
    while slot.lock().unwrap().active == 0 {
        assert!(
            Instant::now() < deadline,
            "history request did not acquire its native lease"
        );
        std::thread::yield_now();
    }
    assert!(receiver.try_recv().is_err());
    assert_eq!(
        run(id, &slot, json!({"type":"storage_status"}))["status"],
        "ok"
    );
    assert_eq!(
        command(id, json!({"type":"runtime_status","runtime_id":slot.id}))["status"],
        "ok"
    );
    assert_eq!(
        command(id, json!({"type":"runtime_close","runtime_id":slot.id}))["status"],
        "ok"
    );
    assert_eq!(
        command(id, json!({"type":"runtime_free","runtime_id":slot.id}))["error"]["code"],
        "busy"
    );
    drop(gate);
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(5)).unwrap()["result"],
        Value::Null
    );
    reader.join().unwrap();
    drop(owner);
    finish_native(id, slot);
}

/// Public worker reconstruction preserves original receipts, checks delivery capacity first and never undoes closure.
/// 公开写入者重建保留原回执、先检查交付容量，且绝不撤销关闭。
#[test]
fn ffi_embedded_persistence_worker_recovery_preserves_original_ownership() {
    for after_storage in [false, true] {
        // Every case uses a real separate native runtime and actual SQLite worker.
        // 每个场景使用真实独立原生运行时及实际 SQLite 工作线程。
        let directory = Directory::new();
        // Commands cross the same public C transport as language SDKs.
        // 命令经过与语言 SDK 相同的公开 C 传输。
        let id = transport();
        // Explicit persistence creates the original native storage owner.
        // 显式持久化创建原原生存储所有者。
        let slot = native(id, directory.config());
        assert_eq!(
            run(id, &slot, json!({"type":"storage_worker_recover"}))["result"],
            false
        );
        // Only fault setup borrows Rust ownership; all administrative recovery uses the dispatcher.
        // 仅故障设置借用 Rust 所有权；全部管理恢复使用分发器。
        let owner = Arc::clone(slot.lock().unwrap().persistence.as_ref().unwrap());
        owner.writer.panic_next_write_for_test(after_storage);
        // The old receipt remains alive through actual reconstruction and retains its original charge.
        // 旧回执跨实际重建存活，并保留原费用。
        let receipt = owner
            .writer
            .submit("original-runtime", None, completed())
            .unwrap();
        // A completed failure is not a promise that the original transaction rolled back.
        // 完成失败不是原事务已回滚的保证。
        let failed = receipt.wait(Duration::from_secs(5)).unwrap();
        assert!(failed.error.is_some());
        // Bound only test observation; actual thread exit remains authoritative.
        // 仅约束测试观察；实际线程退出保持权威。
        let deadline = Instant::now() + Duration::from_secs(5);
        while !owner.writer.status().unwrap().worker_exited {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        // Deliberately reject response allocation before any administrative mutation can occur.
        // 在任何管理变更发生前，有意拒绝响应分配。
        assert!(
            crate::ffi_embedded::control::execute(
                &slot,
                crate::ffi_embedded::commands::RuntimeCommand::StorageWorkerRecover {},
                1
            )
            .is_err()
        );
        assert!(owner.writer.status().unwrap().failure.is_some());
        assert!(owner.writer.status().unwrap().worker_exited);
        // Original receipt quota remains part of the actual writer status exposed through C.
        // 原回执配额仍属于通过 C 暴露的实际写入者状态。
        let before = run(id, &slot, json!({"type":"storage_status"}))["result"].clone();
        assert_eq!(
            run(id, &slot, json!({"type":"storage_worker_recover"}))["result"],
            true
        );
        assert_eq!(
            run(id, &slot, json!({"type":"storage_worker_recover"}))["result"],
            false
        );
        // Rebuilding does not rewrite the old receipt or replay its original write.
        // 重建不改写旧回执，也不重放其原写入。
        let after = run(id, &slot, json!({"type":"storage_status"}))["result"].clone();
        assert_eq!(before["pending_writes"], after["pending_writes"]);
        assert_eq!(before["pending_bytes"], after["pending_bytes"]);
        assert!(after["failure"].is_null());
        assert_eq!(receipt.snapshot().unwrap().error, failed.error);
        assert_eq!(
            owner
                .journal
                .get("original-runtime", "historical-operation")
                .unwrap()
                .is_some(),
            after_storage
        );
        drop(receipt);
        owner.writer.request_close();
        assert_eq!(
            run(id, &slot, json!({"type":"storage_worker_recover"}))["error"]["code"],
            "closed"
        );
        drop(owner);
        finish_native(id, slot);
    }
}

/// Explicit native storage recovery preserves committed evidence and stale revisions never erase it.
/// 显式原生存储恢复保留已提交证据，过期修订绝不抹除它。
#[test]
fn ffi_embedded_persistence_native_recovery_and_history_revision_are_exact() {
    // Both outcomes exercise the same live native runtime with different actual SQLite results.
    // 两种结果使用同一类活动原生运行时，验证不同实际 SQLite 结果。
    for committed in [false, true] {
        // The private root remains valid until final native connection release.
        // 私有根目录一直有效到最后原生连接释放。
        let directory = Directory::new();
        // Public requests are used for all recovery and history operations.
        // 全部恢复和历史操作使用公开请求。
        let id = transport();
        // Initialize actual storage ownership under the known slot.
        // 在已知槽下初始化实际存储所有权。
        let slot = native(id, directory.config());
        // Only failure setup touches the actual journal directly.
        // 仅故障设置直接接触实际日志。
        let owner = Arc::clone(slot.lock().unwrap().persistence.as_ref().unwrap());
        owner.journal.lose_next_confirmation_for_test(committed);
        assert!(
            owner
                .journal
                .insert("original-runtime", &completed())
                .is_err()
        );
        assert_eq!(
            run(id, &slot, json!({"type":"history_next"}))["status"],
            "error"
        );
        assert_eq!(
            run(id, &slot, json!({"type":"storage_recover"}))["result"],
            true
        );
        assert_eq!(
            run(id, &slot, json!({"type":"storage_recover"}))["result"],
            false
        );
        // History remains in its original namespace and only records a real committed transaction.
        // 历史保留在原命名空间，仅记录真实已提交事务。
        let row = run(id, &slot, json!({"type":"history_next"}))["result"].clone();
        if committed {
            assert_eq!(row["runtime_id"], "original-runtime");
            assert_eq!(row["snapshot"]["value"], json!({"original":true}));
            assert_eq!(
                run(
                    id,
                    &slot,
                    json!({"type":"history_next","after":{"runtime_id":"original-runtime","operation_id":"historical-operation"}})
                )["result"],
                Value::Null
            );
            assert_eq!(
                run(
                    id,
                    &slot,
                    json!({"type":"history_forget","history_runtime_id":"original-runtime","operation_id":"historical-operation","expected_revision":2})
                )["error"]["code"],
                "stale_generation"
            );
            assert_eq!(
                run(
                    id,
                    &slot,
                    json!({"type":"history_forget","history_runtime_id":"original-runtime","operation_id":"historical-operation","expected_revision":row["revision"]})
                )["status"],
                "ok"
            );
            assert_eq!(
                run(id, &slot, json!({"type":"history_next"}))["result"],
                Value::Null
            );
        } else {
            assert_eq!(row, Value::Null);
        }
        drop(owner);
        finish_native(id, slot);
    }
}
