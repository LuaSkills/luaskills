//! Trusted host reconciliation is separate immutable evidence, never a replacement execution snapshot.
//! 可信宿主对账是独立不可变证据，绝非替代执行快照。

use super::*;
use crate::runtime::embedded::effects::merge_effects;
use std::collections::BTreeSet;

/// Explicit resolved effects; unknown outcomes remain unreconciled instead of being coerced into success.
/// 显式已解决副作用；未知结果保持未对账，不强制转为成功。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub enum ResolvedEffectState {
    /// Trusted evidence proves the business effect never started.
    /// 可信证据证明业务副作用从未开始。
    NotStarted,
    /// Trusted evidence proves no externally visible mutation applies.
    /// 可信证据证明不存在适用的外部可见变更。
    NotApplicable,
    /// The original external transaction is proven committed.
    /// 原外部事务已证实提交。
    Committed,
    /// The original external transaction is proven rolled back.
    /// 原外部事务已证实回滚。
    RolledBack,
}

impl From<ResolvedEffectState> for EffectState {
    /// Map resolved `value` to the existing effect authority without introducing a second merge policy.
    /// 将已解决的 `value` 映射到既有副作用权威，不引入第二套合并策略。
    fn from(value: ResolvedEffectState) -> Self {
        match value {
            ResolvedEffectState::NotStarted => Self::NotStarted,
            ResolvedEffectState::NotApplicable => Self::NotApplicable,
            ResolvedEffectState::Committed => Self::Committed,
            ResolvedEffectState::RolledBack => Self::RolledBack,
        }
    }
}

/// Historical execution closure asserted by the trusted host after actual owners have stopped.
/// 实际所有者停止后，由可信宿主断言的历史执行关闭。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub enum ReconciledExecution {
    /// The unchanged original snapshot already contains an observed terminal execution result.
    /// 未改变的原始快照已包含观测到的终态执行结果。
    ObservedTerminal,
    /// Actual owners stopped without a durable terminal result; the original nonterminal snapshot stays unchanged.
    /// 实际所有者停止但没有持久终态结果；原非终态快照保持不变。
    StoppedWithoutResult,
}

/// Resolution for one exact original host effect; neither registration nor caller identity can be supplied anew.
/// 一个精确原宿主副作用的结论；不得重新提供注册或调用方身份。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct HostEffectReconciliation {
    /// Exact effect identity from the original snapshot, in the same order as its original records.
    /// 原始快照中的精确副作用身份，顺序与其原始记录相同。
    pub effect_id: String,
    /// Proven final outcome of this original effect, never a retry's outcome.
    /// 此原始副作用的已证实最终结果，绝非重试结果。
    pub effects: ResolvedEffectState,
    /// Nonempty host audit or transaction-query reference; credentials and business payloads do not belong here.
    /// 非空宿主审计或事务查询引用；此处不应包含凭证及业务载荷。
    pub evidence: String,
}

/// One bounded, final, host-authored attestation covering execution closure and every retained effect.
/// 一份有界、最终且由宿主编写的证明，覆盖执行关闭及每个保留副作用。
/// This API does not authenticate the attestation; the embedding host must authorize the resolver and verify evidence.
/// 此 API 不认证证明；嵌入宿主必须授权对账者并核验证据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct OperationReconciliation {
    /// Stable host-assigned resolution identity, retained unchanged across observation or storage retries.
    /// 宿主分配的稳定对账身份，跨观测或存储重试保持不变。
    pub resolution_id: String,
    /// Authorized host resolver identity, not a plugin-supplied authority claim or an authentication credential.
    /// 已授权宿主对账者身份，不是插件提供的权限声明或认证凭证。
    pub resolver: String,
    /// Nonempty evidence reference proving owner closure and the whole operation's external-effect conclusion.
    /// 非空证据引用，证明所有者关闭及整个操作的外部副作用结论。
    pub evidence: String,
    /// Closure evidence consistent with the unchanged original execution phase.
    /// 与未改变原执行阶段一致的关闭证据。
    pub execution: ReconciledExecution,
    /// Resolved aggregate covering both recorded callbacks and any other effects from the original Lua execution.
    /// 已解决的聚合结论，覆盖记录回调及原 Lua 执行的其他副作用。
    pub effects: ResolvedEffectState,
    /// Exactly one resolution per original effect, preserving original order and known outcomes.
    /// 每个原始副作用精确一个结论，保留原始顺序及已知结果。
    pub host_effects: Vec<HostEffectReconciliation>,
}

impl OperationReconciliation {
    /// Validate this complete attestation against unchanged `snapshot`; reject missing evidence and contradictions.
    /// 对照未改变的 `snapshot` 校验此完整证明；拒绝证据缺失及矛盾。
    /// Actual external truth and stopped-owner proof remain responsibilities of the authorized host.
    /// 实际外部事实和所有者已停止的证明仍由已授权宿主负责。
    pub(super) fn validate(&self, snapshot: &OperationSnapshot) -> EmbeddedResult<()> {
        if self.resolution_id.trim().is_empty()
            || self.resolver.trim().is_empty()
            || self.evidence.trim().is_empty()
            || (self.execution == ReconciledExecution::ObservedTerminal)
                != snapshot.phase.is_terminal()
            || self.host_effects.len() != snapshot.host_effects.len()
        {
            return Err(EmbeddedError::invalid(
                "reconciliation requires complete evidence and matching original execution closure",
            ));
        }
        // Known original observations cannot be reversed by a later administrative claim.
        // 后续管理声明不能推翻原已知观测。
        let resolved = EffectState::from(self.effects);
        if snapshot.effects != EffectState::Unknown && snapshot.effects != resolved {
            return Err(EmbeddedError::invalid(
                "reconciliation contradicts known operation effects",
            ));
        }
        // Clone only the bounded records for the existing merge policy; stored observations remain unchanged.
        // 仅为既有合并策略克隆有界记录；存储观测保持不变。
        let mut projected = snapshot.host_effects.clone();
        // Duplicate original or submitted identities cannot make one resolution cover two effects.
        // 重复原始或提交身份不能使一个结论覆盖两个副作用。
        let mut identities = BTreeSet::new();
        for (original, resolution) in projected.iter_mut().zip(&self.host_effects) {
            if original.effect_id.is_empty()
                || original.effect_id != resolution.effect_id
                || !identities.insert(original.effect_id.as_str())
                || resolution.evidence.trim().is_empty()
                || (original.effects != EffectState::Unknown
                    && original.effects != EffectState::from(resolution.effects))
            {
                return Err(EmbeddedError::invalid(
                    "reconciliation must preserve every original effect identity and known outcome",
                ));
            }
            original.effects = resolution.effects.into();
            original.phase = HostEffectPhase::Completed;
        }
        if merge_effects(resolved, &projected) != resolved {
            return Err(EmbeddedError::invalid(
                "reconciliation aggregate contradicts its resolved host effects",
            ));
        }
        Ok(())
    }
}

/// Decode an explicitly present optional reconciliation; missing fields are invalid in this journal format.
/// 解码显式存在的可选对账；此日志格式中缺失字段无效。
pub(super) fn present_reconciliation<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<OperationReconciliation>, D::Error> {
    Option::<OperationReconciliation>::deserialize(deserializer)
}

impl OperationJournal {
    /// Attach final `resolution` to exact original identity at `expected_revision`, returning its durable revision.
    /// 在 `expected_revision` 处为精确原身份附加最终 `resolution`，返回其持久修订。
    /// The trusted caller must prove all original execution and external owners stopped before calling this blocking API.
    /// 可信调用方必须在调用此阻塞 API 前证明全部原执行及外部所有者已停止。
    /// Exact retries acknowledge the same successor; every different attestation or revision is a conflict.
    /// 精确重试确认同一后继；任何不同证明或修订均为冲突。
    pub fn reconcile(
        &self,
        runtime_id: &str,
        operation_id: &str,
        expected_revision: u64,
        resolution: &OperationReconciliation,
    ) -> EmbeddedResult<u64> {
        self.validate_key(runtime_id, operation_id)?;
        json_size(resolution, self.config.max_record_bytes)?;
        if expected_revision == 0 || expected_revision >= i64::MAX as u64 {
            return Err(EmbeddedError::invalid(
                "operation history revision is exhausted or invalid",
            ));
        }
        // One successor revision is shared by first commit and exact lost-confirmation acknowledgement.
        // 首次提交及精确确认丢失后的确认共享唯一后继修订。
        let revision = expected_revision + 1;
        self.transaction(|connection| {
            // The original record is selected under the same transaction as the final attestation write.
            // 原始记录在与最终证明写入相同的事务中选取。
            let mut record = self.read(connection, runtime_id, operation_id)?.ok_or_else(|| {
                EmbeddedError::new(EmbeddedErrorCode::NotFound, "operation history was not found")
            })?;
            if record.revision == revision && record.reconciliation.as_ref() == Some(resolution) {
                return Ok(());
            }
            if record.revision != expected_revision {
                return Err(EmbeddedError::new(EmbeddedErrorCode::StaleGeneration, "operation history revision changed"));
            }
            if record.reconciliation.is_some() {
                return Err(EmbeddedError::new(EmbeddedErrorCode::AlreadyCompleted, "operation history already has a final reconciliation"));
            }
            resolution.validate(&record.snapshot)?;
            record.revision = revision;
            record.reconciliation = Some(resolution.clone());
            // Original snapshot, caller and effect identities are never rewritten by reconciliation.
            // 对账绝不重写原始快照、调用方和副作用身份。
            let document = self.encode(&record)?;
            connection.execute("UPDATE operations SET revision=?3, document=?4, digest=?5 WHERE runtime_id=?1 AND operation_id=?2",
                params![runtime_id, operation_id, revision as i64, document, Sha256::digest(&document).as_slice()]).map_err(storage::error)?;
            Ok(())
        })?;
        Ok(revision)
    }
}
