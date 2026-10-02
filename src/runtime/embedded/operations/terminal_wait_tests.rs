//! Runtime-independent asynchronous observation of the original terminal operation state.
//! 不依赖运行时地异步观测原始终态操作状态。

use super::*;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, Wake, Waker};

/// Count actual observer wakes without creating an executor or timer.
/// 不创建执行器或计时器地统计观察者真实唤醒。
#[derive(Default)]
pub(super) struct CountWake(pub(super) AtomicUsize);

impl Wake for CountWake {
    /// Consume this shared waker and record one synchronous notification; returns no value.
    /// 消费此共享唤醒器并记录一次同步通知；不返回值。
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    /// Borrow this shared waker and record one synchronous notification; returns no value.
    /// 借用此共享唤醒器并记录一次同步通知；不返回值。
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// Admit one memory-only operation using existing fixture limits, returning its observer and unique owner.
/// 使用既有夹具上限接纳一个仅内存操作，返回观察者及唯一所有者。
fn admit() -> (OperationHandle, OperationOwner) {
    // Existing runtime limits stay authoritative for all observation cases.
    // 既有运行时上限继续作为全部观测场景的权威。
    let registry = OperationRegistry::new(
        "async-observation".into(),
        &crate::runtime::embedded::tests::config(),
    )
    .unwrap();
    registry
        .admit(Arc::new(CallControl::new(Duration::from_secs(10)).unwrap()))
        .unwrap()
}

/// Successful publication before the first poll returns the original value without an ambient runtime.
/// 首次轮询前成功发布，无需环境运行时即可返回原始值。
#[test]
fn embedded_operation_async_terminal_already_completed_without_runtime() {
    // Publish before creating an observer to exercise the original snapshot authority.
    // 创建观察者前发布，以验证原始快照权威。
    let (handle, mut owner) = admit();
    owner.advance(OperationPhase::Cleaning).unwrap();
    owner
        .complete(Ok(Value::Null), EffectState::Committed)
        .unwrap();
    // A standard no-op waker and manual poll require neither a timer nor Tokio runtime.
    // 标准空唤醒器及手动轮询既不要求计时器，也不要求 Tokio 运行时。
    let mut waiter = std::pin::pin!(handle.wait_terminal());
    let Poll::Ready(Ok(snapshot)) = waiter
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    else {
        panic!("an already terminal operation must be immediately ready");
    };
    assert_eq!(snapshot.phase, OperationPhase::Succeeded);
    assert_eq!(snapshot.value, Some(Value::Null));
    assert_eq!(snapshot.effects, EffectState::Committed);
}

/// Independent observers all wake on the same true terminal publication, never on phase or cancel intent.
/// 独立观察者均因同一真实终态发布而唤醒，绝不因阶段或取消意图唤醒。
#[test]
fn embedded_operation_async_terminal_wakes_all_observers_only_at_completion() {
    // Two exact observer registrations share only the original operation authority.
    // 两个精确观察者登记仅共享原始操作权威。
    let (handle, mut owner) = admit();
    let first_count = Arc::new(CountWake::default());
    let second_count = Arc::new(CountWake::default());
    let first_waker = Waker::from(Arc::clone(&first_count));
    let second_waker = Waker::from(Arc::clone(&second_count));
    let mut first = std::pin::pin!(handle.wait_terminal());
    let mut second = std::pin::pin!(handle.wait_terminal());
    assert!(
        first
            .as_mut()
            .poll(&mut Context::from_waker(&first_waker))
            .is_pending()
    );
    assert!(
        second
            .as_mut()
            .poll(&mut Context::from_waker(&second_waker))
            .is_pending()
    );
    owner.advance(OperationPhase::Running).unwrap();
    assert!(handle.cancel().unwrap());
    owner.advance(OperationPhase::Cleaning).unwrap();
    assert_eq!(first_count.0.load(Ordering::SeqCst), 0);
    assert_eq!(second_count.0.load(Ordering::SeqCst), 0);
    owner
        .complete(Ok(Value::Bool(true)), EffectState::Committed)
        .unwrap();
    assert_eq!(first_count.0.load(Ordering::SeqCst), 1);
    assert_eq!(second_count.0.load(Ordering::SeqCst), 1);
    for (mut waiter, waker) in [
        (first.as_mut(), &first_waker),
        (second.as_mut(), &second_waker),
    ] {
        let Poll::Ready(Ok(snapshot)) = waiter.as_mut().poll(&mut Context::from_waker(waker))
        else {
            panic!("every terminal observer must receive the same acknowledged outcome");
        };
        assert_eq!(snapshot.value, Some(Value::Bool(true)));
        assert!(snapshot.cancellation_requested);
    }
}

/// Dropping one registered observer leaves real execution and every other observer intact.
/// 丢弃一个已登记观察者，保持真实执行及其余观察者完整。
#[test]
fn embedded_operation_async_terminal_dropped_observer_does_not_cancel() {
    // Owned pins let the test drop the actual future rather than only a pinned reference.
    // 拥有所有权的固定指针使测试丢弃实际 Future，而非仅丢弃固定引用。
    let (handle, mut owner) = admit();
    let dropped_count = Arc::new(CountWake::default());
    let retained_count = Arc::new(CountWake::default());
    let dropped_waker = Waker::from(Arc::clone(&dropped_count));
    let retained_waker = Waker::from(Arc::clone(&retained_count));
    let mut dropped = Box::pin(handle.wait_terminal());
    let mut retained = Box::pin(handle.wait_terminal());
    assert!(
        dropped
            .as_mut()
            .poll(&mut Context::from_waker(&dropped_waker))
            .is_pending()
    );
    assert!(
        retained
            .as_mut()
            .poll(&mut Context::from_waker(&retained_waker))
            .is_pending()
    );
    drop(dropped);
    assert!(!owner.control().is_cancelled());
    assert_eq!(handle.snapshot().unwrap().phase, OperationPhase::Queued);
    owner.advance(OperationPhase::Cleaning).unwrap();
    owner
        .complete(Ok(Value::Null), EffectState::NotApplicable)
        .unwrap();
    assert_eq!(dropped_count.0.load(Ordering::SeqCst), 0);
    assert_eq!(retained_count.0.load(Ordering::SeqCst), 1);
    assert!(
        retained
            .as_mut()
            .poll(&mut Context::from_waker(&retained_waker))
            .is_ready()
    );
}

/// Concurrent registration and real publication cannot leave a pending observer without a wake.
/// 并发登记及真实发布不能留下未获唤醒的待完成观察者。
#[test]
fn embedded_operation_async_terminal_registration_publication_race() {
    // A finite race supplements the deterministic before-poll and after-registration cases above.
    // 有限竞态补充上方确定的轮询前及登记后场景。
    for _ in 0..64 {
        let (handle, mut owner) = admit();
        owner.advance(OperationPhase::Cleaning).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let publisher_barrier = Arc::clone(&barrier);
        let publisher = std::thread::spawn(move || {
            publisher_barrier.wait();
            owner
                .complete(Ok(Value::Null), EffectState::NotApplicable)
                .unwrap();
        });
        let count = Arc::new(CountWake::default());
        let waker = Waker::from(Arc::clone(&count));
        let mut waiter = std::pin::pin!(handle.wait_terminal());
        barrier.wait();
        let first = waiter.as_mut().poll(&mut Context::from_waker(&waker));
        publisher.join().unwrap();
        if first.is_pending() {
            assert!(
                count.0.load(Ordering::SeqCst) > 0,
                "a pending registered observer must be woken"
            );
            assert!(
                waiter
                    .as_mut()
                    .poll(&mut Context::from_waker(&waker))
                    .is_ready()
            );
        }
    }
}

/// Inspect every publicly reachable operation lock during synchronous waker reentry.
/// 在同步唤醒器重入时检查每个可访问的操作锁。
struct ReentrantWake {
    /// Original client view used for the real snapshot projection.
    /// 用于真实快照投影的原始客户端视图。
    handle: OperationHandle,
    /// Number of successful lock-free terminal observations.
    /// 成功无锁终态观测的次数。
    observations: AtomicUsize,
}

impl Wake for ReentrantWake {
    /// Consume this waker, query its original snapshot, and record successful reentry; returns no value.
    /// 消费此唤醒器，查询原始快照并记录成功重入；不返回值。
    fn wake(self: Arc<Self>) {
        assert!(self.handle.operation.snapshot.try_lock().is_ok());
        assert!(self.handle.operation.transition.try_lock().is_ok());
        assert!(self.handle.snapshot().unwrap().phase.is_terminal());
        self.observations.fetch_add(1, Ordering::SeqCst);
    }
}

/// Direct completion invokes custom wakers only after operation, transition and effect locks are released.
/// 直接完成仅在操作、变更及副作用锁释放后调用自定义唤醒器。
#[test]
fn embedded_operation_async_terminal_direct_waker_reentry() {
    // The real snapshot also projects the effect ledger during synchronous reentry.
    // 同步重入期间，真实快照还会投影副作用账本。
    let (handle, mut owner) = admit();
    let probe = Arc::new(ReentrantWake {
        handle: handle.clone(),
        observations: AtomicUsize::new(0),
    });
    let waker = Waker::from(Arc::clone(&probe));
    let mut waiter = std::pin::pin!(handle.wait_terminal());
    assert!(
        waiter
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );
    owner.advance(OperationPhase::Cleaning).unwrap();
    owner
        .complete(Ok(Value::Null), EffectState::NotApplicable)
        .unwrap();
    assert_eq!(probe.observations.load(Ordering::SeqCst), 1);
    assert!(
        waiter
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_ready()
    );
}

/// Deliberately panics when the standard notification invokes a caller-owned waker.
/// 标准通知调用调用方自有唤醒器时刻意 panic。
struct PanicWake;

impl Wake for PanicWake {
    /// Consume this waker and panic; returns no normal value.
    /// 消费此唤醒器并 panic；不正常返回值。
    fn wake(self: Arc<Self>) {
        panic!("caller-owned waker panic probe");
    }
}

/// Standard Notify propagates waker panics after terminal publication without poisoning operation locks.
/// 标准 Notify 在终态发布后传播唤醒器 panic，且不使操作锁中毒。
#[test]
fn embedded_operation_async_terminal_waker_panic_is_not_automatic_recovery() {
    // This test records the trust boundary; production notification deliberately adds no panic wrapper.
    // 此测试记录信任边界；生产通知刻意不添加 panic 包装。
    let (handle, mut owner) = admit();
    let waker = Waker::from(Arc::new(PanicWake));
    let mut waiter = std::pin::pin!(handle.wait_terminal());
    assert!(
        waiter
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );
    owner.advance(OperationPhase::Cleaning).unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        owner
            .complete(Ok(Value::Null), EffectState::NotApplicable)
            .unwrap();
    }));
    assert!(result.is_err(), "Tokio propagates the custom waker panic");
    assert_eq!(handle.snapshot().unwrap().phase, OperationPhase::Succeeded);
    assert!(!handle.operation.transition.is_poisoned());
    assert!(owner.pending_completion().is_none());
}
