use super::*;
use crate::ffi_embedded::tests::runtimes::{engine_options, runtime_config};
use std::sync::Barrier;
use std::time::{Duration, Instant};

/// Construct an actual empty core using the same options as the native protocol fixture.
/// 使用与原生协议夹具相同的选项构造实际空核心。
fn core() -> EmbeddedRuntime {
    EmbeddedRuntime::new(
        Arc::new(LuaEngine::new(engine_options()).unwrap()),
        runtime_config(),
    )
    .unwrap()
}

/// Await actual core shutdown, not merely publication of the close flag.
/// 等待实际核心关闭，而不只是关闭标志发布。
fn wait_closed(slot: &Arc<RuntimeSlot>) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !slot.snapshot().unwrap().closed {
        assert!(Instant::now() < deadline, "native workers did not drain");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Concurrent close marks an initializing identity immediately and closes the future core before releasing creation ownership.
/// 并发关闭立即标记初始化中身份，并在释放创建所有权前关闭未来核心。
#[test]
fn ffi_embedded_runtime_close_during_initialization_retains_actual_owner() {
    let slot = RuntimeSlot::new().unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let creator_slot = Arc::clone(&slot);
    let creator_barrier = Arc::clone(&barrier);
    let creator = std::thread::spawn(move || {
        creator_slot.initialize_with(|| {
            creator_barrier.wait();
            creator_barrier.wait();
            Ok(core())
        })
    });
    barrier.wait();
    assert_eq!(
        slot.snapshot().unwrap().initialization,
        InitializationPhase::Initializing
    );
    assert_eq!(
        slot.initialize_with(|| unreachable!()).unwrap_err().code,
        EmbeddedErrorCode::Busy
    );
    slot.request_close().unwrap();
    assert!(slot.snapshot().unwrap().closing);
    assert!(!slot.snapshot().unwrap().closed);
    assert_eq!(slot.release().unwrap_err().code, EmbeddedErrorCode::Busy);
    barrier.wait();
    creator.join().unwrap().unwrap();
    wait_closed(&slot);
    slot.release().unwrap();
    assert_eq!(
        slot.request_close().unwrap_err().code,
        EmbeddedErrorCode::Closed
    );
}

/// Even joined workers cannot make a slot releasable while another native call retains its core reference.
/// 即使工作线程已汇合，其他原生调用仍保留核心引用时，槽也不能释放。
#[test]
fn ffi_embedded_runtime_release_waits_for_actual_native_user_references() {
    let slot = RuntimeSlot::new().unwrap();
    slot.initialize_with(|| Ok(core())).unwrap();
    let lease = slot.lease(&mut slot.lock().unwrap()).unwrap();
    slot.request_close().unwrap();
    wait_closed(&slot);
    assert_eq!(slot.release().unwrap_err().code, EmbeddedErrorCode::Busy);
    drop(lease);
    slot.release().unwrap();
    assert_eq!(
        slot.snapshot().err().unwrap().code,
        EmbeddedErrorCode::Closed
    );
}

/// An unwound construction is explicitly faulted and never presented as evidence that library unloading is safe.
/// 栈展开构造明确标记为故障，绝不作为动态库可安全卸载的证据。
#[test]
fn ffi_embedded_runtime_initialization_panic_blocks_unproven_unload() {
    let slot = RuntimeSlot::new().unwrap();
    slot.initialize_with(|| panic!("intentional construction failure"))
        .unwrap();
    slot.request_close().unwrap();
    let snapshot = slot.snapshot().unwrap();
    assert_eq!(snapshot.initialization, InitializationPhase::Faulted);
    assert!(!snapshot.closed);
    assert_eq!(snapshot.error.unwrap().code, EmbeddedErrorCode::Internal);
    assert_eq!(
        slot.release().unwrap_err().code,
        EmbeddedErrorCode::Internal
    );
}
