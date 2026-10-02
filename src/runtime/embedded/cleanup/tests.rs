//! Internal completion notification tests cover original lock order and weak observer lifetime, not VM destruction.
//! 内部完成通知测试覆盖原锁序及弱观察者生命周期，不验证 VM 销毁。

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;

/// One failure ceiling bounds coordination waits without asserting completion latency.
/// 单个失败上限限制协调等待，不断言完成延迟。
const FAILURE_BOUND: Duration = Duration::from_secs(5);

/// One original observer pairs its completion predicate with its own real condition variable.
/// 单个原观察者将其完成谓词与自身真实条件变量配对。
#[derive(Default)]
struct Observer {
    /// The callback sets this signal under the same gate released atomically by the waiter.
    /// 回调在等待者原子释放的相同闸门下设置此信号。
    woken: Mutex<bool>,
    /// Only this original callback can wake this original waiter's condition variable.
    /// 仅此原回调可以唤醒此原等待者的条件变量。
    changed: Condvar,
}

/// Exercise registration before publication or after Completed according to completed_before_registration.
/// 根据 completed_before_registration 验证发布前登记或完成后登记。
/// Return unit after the original callback proves unlocked receipt metadata and wakes the controlled waiter.
/// 原回调证明回执元数据已解锁并唤醒受控等待者后返回空值。
fn assert_original_completion_wake(completed_before_registration: bool) {
    // This internal receipt contains only lifetime evidence and never represents a constructed VM.
    // 此内部回执仅包含生命周期证据，绝不代表已构造 VM。
    let receipt = ModuleRetirement::new(String::from("original-completion-observation"));
    if completed_before_registration {
        receipt.publish(ModuleRetirementPhase::Completed, None);
    }
    // Retain one actual observer for the test's original waiting thread.
    // 为测试原等待线程保留单个实际观察者。
    let observer = Arc::new(Observer::default());
    // Registration retains only weak observation identity and cannot own the observer or receipt.
    // 登记仅保留弱观测身份，不能拥有观察者或回执。
    let weak_observer = Arc::downgrade(&observer);
    // A weak receipt probe can check lock order without creating an evidence-to-callback cycle.
    // 弱回执探针可检查锁序，不建立证据到回调的引用环。
    let weak_receipt = Arc::downgrade(&receipt.evidence);
    // Report the true metadata lock result before trying the observer gate, even for a broken publisher.
    // 即使发布器有缺陷，也在尝试观察者闸门前报告真实元数据锁结果。
    let (checked_sender, checked_receiver) = mpsc::channel();
    // This callback always proceeds to wake, so a failed lock-order assertion cannot strand the waiter.
    // 此回调始终继续唤醒，避免失败锁序断言使等待者滞留。
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        // Actual publication or registration still owns this exact evidence while the callback runs.
        // 回调运行时，实际发布或登记仍拥有此精确证据。
        let evidence = weak_receipt
            .upgrade()
            .expect("original receipt remains owned");
        // A nonblocking acquisition records lock inversion without deadlocking inside a broken callback.
        // 非阻塞获取记录锁反转，不在缺陷回调内部死锁。
        let unlocked = evidence.snapshot.try_lock().is_ok();
        checked_sender
            .send(unlocked)
            .expect("original lock-order observation receiver");
        // Upgrade lasts only for this original notification and is released when the callback returns.
        // 升级仅持续此原通知，在回调返回时释放。
        let original = weak_observer
            .upgrade()
            .expect("original waiter remains owned");
        // The publisher must serialize against the waiter's real atomic unlock-and-wait transition.
        // 发布器必须与等待者真实原子解锁及等待转换排序。
        let mut woken = original.woken.lock().expect("original observer gate");
        *woken = true;
        original.changed.notify_all();
    });
    if !completed_before_registration {
        receipt
            .wake_on_completion(Arc::clone(&wake))
            .expect("register original completion observer");
    }

    // Readiness is sent only while the waiter owns the original gate.
    // 仅等待者拥有原闸门时发送就绪。
    let (ready_sender, ready_receiver) = mpsc::channel();
    // This explicit handoff releases the waiter into Condvar only after the callback has entered.
    // 此明确交接仅在回调已进入后才允许等待者进入条件变量。
    let (sleep_sender, sleep_receiver) = mpsc::channel();
    // The waiting thread retains the same original observer, rather than an unrelated notification target.
    // 等待线程保留相同原观察者，而非无关通知目标。
    let waiting_observer = Arc::clone(&observer);
    // This actual waiter forces publication before sleep while preserving the required gate ordering.
    // 此实际等待者强制发布先于入睡，同时保持要求的闸门顺序。
    let waiter = std::thread::spawn(move || {
        // Keep the original gate through the channel handoff; the callback cannot notify until Condvar releases it.
        // 跨通道交接保持原闸门；条件变量释放前，回调无法通知。
        let woken = waiting_observer.woken.lock().expect("original waiter gate");
        ready_sender.send(()).expect("original waiter readiness");
        sleep_receiver
            .recv_timeout(FAILURE_BOUND)
            .expect("original waiter sleep permission");
        // The predicate handles spurious wakes; timeout is solely a test failure ceiling.
        // 谓词处理虚假唤醒；超时仅为测试失败上限。
        let (woken, _) = waiting_observer
            .changed
            .wait_timeout_while(woken, FAILURE_BOUND, |woken| !*woken)
            .expect("original observer condition wait");
        assert!(
            *woken,
            "original completion callback did not wake its waiter"
        );
    });
    ready_receiver
        .recv_timeout(FAILURE_BOUND)
        .expect("waiter owns original gate");
    // The original receipt remains the producer in both explicitly selected registration orders.
    // 在两个明确选择的登记顺序中，原回执始终为生产者。
    let publishing_receipt = receipt.clone();
    // Completion-first exercises real late registration; registration-first exercises the real publication callback.
    // 完成优先验证真实晚登记；登记优先验证真实发布回调。
    let publisher = std::thread::spawn(move || {
        if completed_before_registration {
            publishing_receipt
                .wake_on_completion(wake)
                .expect("register after original completion");
        } else {
            publishing_receipt.publish(ModuleRetirementPhase::Completed, None);
        }
    });
    // Delay the assertion until both threads finish, allowing even an incorrect metadata lock order to wake and exit.
    // 将断言延后至两个线程结束，让错误元数据锁序仍可唤醒并退出。
    let metadata_unlocked = checked_receiver
        .recv_timeout(FAILURE_BOUND)
        .expect("actual callback entered before waiter sleep");
    sleep_sender
        .send(())
        .expect("release original gate into wait");
    waiter.join().expect("original waiter completed");
    publisher.join().expect("original producer completed");
    assert!(
        metadata_unlocked,
        "callback ran while receipt metadata was locked"
    );
    assert_eq!(
        receipt.snapshot().expect("original receipt snapshot").phase,
        ModuleRetirementPhase::Completed
    );
}

/// Verify both real registration orders wake the same waiter across controlled publication-before-sleep.
/// 验证两个真实登记顺序跨受控发布先于入睡，唤醒相同等待者。
/// No inputs or return value; failures concern notification and lock order only.
/// 无输入或返回值；失败仅涉及通知及锁序。
#[test]
fn embedded_retirement_completion_wake_handles_early_and_late_registration() {
    assert_original_completion_wake(false);
    assert_original_completion_wake(true);
}

/// A retained original callback must not prolong its weak observer's lifetime.
/// 保留原回调不得延长其弱观察者生命周期。
/// No inputs or return value; publication after observer exit still preserves exact receipt metadata.
/// 无输入或返回值；观察者退出后发布仍保持精确回执元数据。
#[test]
fn embedded_retirement_completion_wake_does_not_retain_weak_observer() {
    // Construct inert internal evidence without claiming physical VM retirement.
    // 构造未启动内部证据，不宣称物理 VM 退役。
    let receipt = ModuleRetirement::new(String::from("weak-original-completion-observation"));
    // Only actual callback executions while this original observer lives increment its counter.
    // 仅此原观察者存活期间的实际回调执行递增计数。
    let observer = Arc::new(AtomicUsize::new(0));
    // This independent weak witness checks actual observer destruction after its last owner exits.
    // 此独立弱见证检查最后所有者退出后实际观察者销毁。
    let weak_observer = Arc::downgrade(&observer);
    // The registered closure captures precisely the same weak identity, with no strong owner.
    // 登记闭包精确捕获相同弱身份，不持有强所有者。
    let callback_observer = weak_observer.clone();
    receipt
        .wake_on_completion(Arc::new(move || {
            // A temporary upgrade exists only when the original observer still has an owner.
            // 仅原观察者仍有所有者时才存在临时升级。
            if let Some(observer) = callback_observer.upgrade() {
                observer.fetch_add(1, Ordering::Relaxed);
            }
        }))
        .expect("original weak completion observer");
    assert_eq!(observer.load(Ordering::Relaxed), 0);
    drop(observer);
    assert!(weak_observer.upgrade().is_none());
    receipt.publish(ModuleRetirementPhase::Completed, None);
    assert!(weak_observer.upgrade().is_none());
    assert_eq!(
        receipt.snapshot().expect("same original receipt").phase,
        ModuleRetirementPhase::Completed
    );
}
