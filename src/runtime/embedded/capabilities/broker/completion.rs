//! Nonblocking durable completion retains the original SDK result and registration admission.
//! 非阻塞持久完成保留原始 SDK 结果及注册入场许可。

use super::*;

impl HostRequestBroker {
    /// Advance currently retained confirmations without executing handlers, waiting for disk, or retrying failures.
    /// 推进当前保留确认，不执行处理器、不等待磁盘，也不重试失败。
    pub(crate) fn maintain_completions(&self) -> EmbeddedResult<()> {
        // Copy only bounded identities; real completion ownership stays with the broker until individually claimed.
        // 仅复制有界身份；真实完成所有权在逐项取得前仍留在代理。
        let identities = self
            .state
            .lock()
            .map_err(|_| poisoned())?
            .records
            .iter()
            .filter(|(_, record)| {
                record.phase == HostRequestPhase::Completing && record.pending_outcome.is_some()
            })
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for identity in identities {
            self.drive_completion(&identity)?;
        }
        Ok(())
    }

    /// Poll exact `id` while exclusively retaining its original prepared invocation and bounded result.
    /// 独占保留原始准备调用及有界结果时，轮询精确 `id`。
    /// A cancelled orphan may expire during another observer's pass; internal maintenance then has nothing to do.
    /// 已取消孤立请求可能在另一观察者遍历期间过期；内部维护此时无需处理。
    pub(super) fn drive_completion(&self, id: &str) -> EmbeddedResult<()> {
        // Claim once under metadata; disk observation and permit destruction happen after releasing this lock.
        // 在元数据下取得一次所有权；释放此锁后才观测磁盘及析构许可。
        let retained = {
            let mut state = self.state.lock().map_err(|_| poisoned())?;
            let Some(record) = state.records.get_mut(id) else {
                return Ok(());
            };
            if record.phase != HostRequestPhase::Completing || record.pending_outcome.is_none() {
                return Ok(());
            }
            // Pending evidence and its actual admission move together into this short-lived drive owner.
            // 待完成证据及真实入场共同移动到此短期推进所有者。
            let prepared = record.prepared.take().ok_or_else(poisoned)?;
            let outcome = record
                .pending_outcome
                .take()
                .expect("validated original completion");
            (prepared, outcome)
        };
        // The original outcome is never supplied by another completion call or reconstructed from status.
        // 原始结果绝不由另一次完成调用提供，也不从状态重建。
        let (prepared, mut outcome) = retained;
        match prepared.effect.poll_outcome() {
            Ok(true) => {
                if let Err(error) = prepared.invocation.authorize() {
                    outcome.result = Err(error);
                }
                // Admission destruction may run native cleanup; it remains outside broker and scheduler metadata.
                // 入场析构可能运行原生清理；它始终处于代理及调度元数据之外。
                drop(prepared);
                self.publish(id, outcome)
            }
            progress => {
                // Cancellation cannot remove a Completing record, so the exact original owner can be restored.
                // 取消不能删除完成中记录，因此可以恢复精确原始所有者。
                let mut state = self.state.lock().map_err(|_| poisoned())?;
                let record = state.records.get_mut(id).ok_or_else(poisoned)?;
                record.prepared = Some(prepared);
                record.pending_outcome = Some(outcome);
                progress.map(|_| ())
            }
        }
    }
}
