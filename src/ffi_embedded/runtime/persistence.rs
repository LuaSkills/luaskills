//! Explicit native ownership for an optional durable runtime and its independent storage worker.
//! 可选持久运行时及其独立存储工作线程的显式原生所有权。

use super::*;
use crate::runtime::embedded::{
    OperationJournal, OperationJournalConfig, OperationJournalWorker, OperationJournalWorkerConfig,
};
use serde::Deserialize;
use std::path::PathBuf;

/// Explicit host storage selection; omitting this whole object selects the existing memory-only runtime.
/// 显式宿主存储选择；省略整个对象表示选择既有纯内存运行时。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub(in crate::ffi_embedded) struct RuntimePersistenceConfig {
    /// Absolute database path owned and protected by the host, never a plugin-selected location.
    /// 由宿主拥有和保护的绝对数据库路径，绝非插件选择位置。
    pub(super) path: PathBuf,
    /// Explicit durable retention limits, independent of transient runtime budgets.
    /// 显式持久保留上限，独立于瞬态运行时预算。
    pub(super) journal: OperationJournalConfig,
    /// Explicit bounded storage-thread receipt limits.
    /// 显式有界存储线程回执上限。
    pub(super) worker: OperationJournalWorkerConfig,
}

/// Retained storage ownership survives failed initialization and all active native leases.
/// 保留的存储所有权跨初始化失败及全部活动原生租借存活。
pub(in crate::ffi_embedded) struct PersistenceOwner {
    /// Exact journal retained by both runtime checkpoints and host history commands.
    /// 运行时检查点与宿主历史命令共同保留的精确日志。
    pub(in crate::ffi_embedded) journal: Arc<OperationJournal>,
    /// One fixed writer whose actual termination is part of the FFI closure barrier.
    /// 单个固定写入者，其实际终止属于 FFI 关闭屏障。
    pub(in crate::ffi_embedded) writer: Arc<OperationJournalWorker>,
}

impl PersistenceOwner {
    /// Open `config` once, returning ownership before constructing any runtime that can use its writer.
    /// 按 `config` 打开一次，在构造可使用其写入者的运行时前返回所有权。
    pub(super) fn new(config: RuntimePersistenceConfig) -> EmbeddedResult<Arc<Self>> {
        config.worker.validate()?;
        // No worker exists if the file or retention declaration fails validation.
        // 文件或保留声明校验失败时不存在工作线程。
        let journal = Arc::new(OperationJournal::open(&config.path, config.journal)?);
        // Thread construction is the final fallible step before returning its retained owner.
        // 返回保留所有者前，线程构造是最后一个可失败步骤。
        let writer = Arc::new(OperationJournalWorker::new(
            Arc::clone(&journal),
            config.worker,
        )?);
        Ok(Arc::new(Self { journal, writer }))
    }

    /// Request storage closure after the core has drained; return true only after real thread and receipt release.
    /// 核心排空后请求存储关闭；仅在真实线程及回执释放后返回真。
    pub(super) fn poll_closed(&self) -> EmbeddedResult<bool> {
        self.writer.request_close();
        self.writer.poll_closed()
    }
}
