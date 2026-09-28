//! Rust-owned package and dependency leases retained across the complete module generation lifetime.
//! 跨完整模块代次生命周期保留的 Rust 自有插件包及依赖租约。

use std::sync::Arc;

/// Opaque host ownership shared by pools belonging to one immutable module generation.
/// 由同一不可变模块代次的多个池共享的不透明宿主所有权。
/// This value grants no Lua capability and is never serialized into an ABI request or operation history.
/// 此值不授予 Lua 能力，也绝不序列化到 ABI 请求或操作历史。
/// The host must bind it to the declared generation and keep any external users independently pinned.
/// 宿主必须将其绑定到声明的代次，并为任何外部使用者独立保留引用。
/// Resource destruction must be nonblocking, nonpanicking, and must not reenter the runtime; perform active cleanup first.
/// 资源析构必须不阻塞、不触发 panic 且不重入运行时；主动清理必须提前执行。
/// Do not retain this resource's owning runtime or pool inside it, which would create a strong reference cycle.
/// 不能在资源内部保留拥有它的运行时或池，否则会形成强引用环。
#[derive(Clone)]
pub struct ModuleResourceOwner {
    /// A real strong owner, not a path, identifier, cleanup receipt, or weak reference.
    /// 真实强所有者，而非路径、标识、清理回执或弱引用。
    _resource: Arc<dyn Send + Sync>,
}

impl ModuleResourceOwner {
    /// Erases the type of the host's real `resource` lease while preserving its strong ownership.
    /// 擦除宿主真实 `resource` 租约的类型，同时保留其强所有权。
    /// Returns a cloneable owner; the resource is released only after all host and runtime owners leave.
    /// 返回可克隆所有者；所有宿主及运行时所有者离开后，资源才被释放。
    pub fn new<T: Send + Sync + 'static>(resource: Arc<T>) -> Self {
        Self {
            _resource: resource,
        }
    }
}
