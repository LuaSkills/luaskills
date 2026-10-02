use std::sync::{Arc, OnceLock, RwLock, Weak};

/// Stable runtime log level emitted by the LuaSkills library.
/// LuaSkills 库发出的稳定运行时日志级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeLogLevel {
    /// Informational runtime event.
    /// 信息级运行时事件。
    Info,
    /// Warning runtime event.
    /// 告警级运行时事件。
    Warn,
    /// Error runtime event.
    /// 错误级运行时事件。
    Error,
}

/// Structured runtime log event forwarded from the library to the host callback.
/// 从库转发到宿主回调的结构化运行时日志事件。
#[derive(Debug, Clone)]
pub struct RuntimeLogEvent {
    /// Stable runtime log level.
    /// 稳定的运行时日志级别。
    pub level: RuntimeLogLevel,
    /// Human-readable log message emitted by the library.
    /// 由库发出的可读日志消息。
    pub message: String,
}

/// Host callback type that receives runtime log events.
/// 接收运行时日志事件的宿主回调类型。
pub type RuntimeLogCallback = Arc<dyn Fn(&RuntimeLogEvent) + Send + Sync + 'static>;

/// Global host log callback shared by the runtime until per-host routing is introduced.
/// 在引入更细粒度宿主路由前，由运行时共享使用的全局宿主日志回调。
static RUNTIME_LOG_CALLBACK: OnceLock<RwLock<Option<RuntimeLogCallback>>> = OnceLock::new();

/// Return the shared runtime log callback container.
/// 返回共享运行时日志回调容器。
fn runtime_log_callback() -> &'static RwLock<Option<RuntimeLogCallback>> {
    RUNTIME_LOG_CALLBACK.get_or_init(|| RwLock::new(None))
}

/// Register or replace the host-side runtime log callback.
/// 注册或替换宿主侧运行时日志回调。
pub fn set_log_callback(callback: Option<RuntimeLogCallback>) {
    if let Ok(mut guard) = runtime_log_callback().write() {
        *guard = callback;
    }
}

/// Snapshot the existing subscriber outside execution, scheduler, pool and receipt locks.
/// 在执行、调度器、池及回执锁之外快照既有订阅者。
/// Return no subscriber when logging is disabled or its configuration lock is poisoned.
/// 日志关闭或其配置锁中毒时返回无订阅者。
pub(crate) fn diagnostic_subscriber() -> Option<DiagnosticSubscriber> {
    runtime_log_callback()
        .read()
        .ok()
        .and_then(|guard| guard.as_ref().map(Arc::downgrade))
}

/// Weak reference to the exact callback prevents diagnostic metadata from extending host ownership.
/// 指向精确回调的弱引用防止诊断元数据延长宿主所有权。
pub(crate) type DiagnosticSubscriber = Weak<dyn Fn(&RuntimeLogEvent) + Send + Sync + 'static>;

/// Resolve subscriber only if it is the exact callback currently registered, outside all runtime locks.
/// 仅 subscriber 是当前注册的精确回调时解析它，且调用必须位于全部运行时锁外。
/// Return an in-flight callback snapshot with the same replacement race semantics as public emit.
/// 返回进行中回调快照，具有与公开 emit 相同的替换竞态语义。
fn current_diagnostic_callback(subscriber: &DiagnosticSubscriber) -> Option<RuntimeLogCallback> {
    // External callback owners cannot keep a removed or replaced diagnostic subscription active.
    // 外部回调所有者不能使已移除或替换的诊断订阅持续活动。
    let original = subscriber.upgrade()?;
    // Clone before releasing the log lock; invocation happens only after its guard is destroyed.
    // 在释放日志锁前克隆；仅保护对象销毁后才执行回调。
    let guard = runtime_log_callback().read().ok()?;
    guard
        .as_ref()
        .filter(|current| Arc::ptr_eq(current, &original))
        .cloned()
}

/// Check current callback identity before measuring a stage; never call while holding runtime locks.
/// 在测量阶段前检查当前回调身份；持有运行时锁时绝不调用。
pub(crate) fn diagnostic_is_current(subscriber: &DiagnosticSubscriber) -> bool {
    current_diagnostic_callback(subscriber).is_some()
}

/// Send lazily constructed private diagnostics through subscriber after measured work and all runtime locks.
/// 在被测工作结束且全部运行时锁释放后，经 subscriber 发送惰性构造的私有诊断。
/// The synchronous subscriber must not block or reenter; its panic cannot change execution or retirement outcomes.
/// 同步订阅者不得阻塞或重入；其 panic 不能改变执行或退役结果。
pub(crate) fn send_diagnostic(subscriber: &DiagnosticSubscriber, message: impl FnOnce() -> String) {
    // A removed or replaced subscriber causes no JSON construction; an acquired snapshot may finish in flight.
    // 已移除或替换订阅者不会导致 JSON 构造；已获取快照可以完成进行中的发送。
    let Some(callback) = current_diagnostic_callback(subscriber) else {
        return;
    };
    // Diagnostic construction and callback execution are outside the measured business interval.
    // 诊断构造及回调执行位于被测业务时间区间之外。
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Preserve the existing public event shape and emit semantics.
        // 保持既有公开事件形状及 emit 语义。
        let event = RuntimeLogEvent {
            level: RuntimeLogLevel::Info,
            message: message(),
        };
        callback(&event);
    }));
}

/// Emit one structured runtime log event to the current host callback if it exists.
/// 若当前宿主回调存在，则向其发送一条结构化运行时日志事件。
pub fn emit(level: RuntimeLogLevel, message: impl Into<String>) {
    let event = RuntimeLogEvent {
        level,
        message: message.into(),
    };
    let callback = runtime_log_callback()
        .read()
        .ok()
        .and_then(|guard| guard.as_ref().cloned());
    if let Some(callback) = callback {
        callback(&event);
    }
}

/// Emit one informational runtime log event.
/// 发送一条信息级运行时日志事件。
pub fn info(message: impl Into<String>) {
    emit(RuntimeLogLevel::Info, message);
}

/// Emit one warning runtime log event.
/// 发送一条告警级运行时日志事件。
pub fn warn(message: impl Into<String>) {
    emit(RuntimeLogLevel::Warn, message);
}

/// Emit one error runtime log event.
/// 发送一条错误级运行时日志事件。
pub fn error(message: impl Into<String>) {
    emit(RuntimeLogLevel::Error, message);
}
