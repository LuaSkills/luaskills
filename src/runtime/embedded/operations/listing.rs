//! Bounded discovery follows actual admission publication, including identities reserved much earlier.
//! 有界发现遵循实际入场发布顺序，包含远早于发布时预留的身份。

use super::*;

/// One page of retained operation identities; status and effects remain queryable by each exact identity.
/// 一页保留操作身份；状态及副作用继续通过各精确身份查询。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct OperationPage {
    /// Identities in admission publication order, independent of reservation and completion order.
    /// 按入场发布顺序排列的身份，独立于预留及完成顺序。
    pub operation_ids: Vec<String>,
    /// Last returned identity, or the unchanged input cursor for an empty page.
    /// 最后返回的身份；空页保留输入游标。
    pub after_operation_id: Option<String>,
    /// More matching retained records existed during this atomic registry observation.
    /// 此次原子注册表观测期间仍存在更多匹配保留记录。
    pub has_more: bool,
}

impl OperationRegistry {
    /// List at most `limit` retained IDs for optional exact `pool_id`, after a still-retained cursor.
    /// 为可选精确 `pool_id` 列出仍保留游标之后至多 `limit` 个保留身份。
    /// Reject zero or over-budget limits and forgotten/foreign cursors; callers restart enumeration after forgetting.
    /// 拒绝零或超预算数量及已遗忘／外来游标；遗忘后调用方重新开始枚举。
    pub fn list(
        &self,
        pool_id: Option<&str>,
        after_operation_id: Option<&str>,
        limit: usize,
    ) -> EmbeddedResult<OperationPage> {
        if limit == 0 || limit > self.max_operations {
            return Err(EmbeddedError::invalid(
                "operation page limit exceeds retained operation budget",
            ));
        }
        if pool_id.is_some_and(|id| id.trim().is_empty() || id.contains('\0')) {
            return Err(EmbeddedError::invalid("operation pool filter is invalid"));
        }
        let state = self.lock()?;
        let after = if let Some(id) = after_operation_id {
            let operation = state.records.get(id).ok_or_else(|| {
                EmbeddedError::new(
                    EmbeddedErrorCode::NotFound,
                    "operation page cursor is not retained",
                )
            })?;
            if !matches_pool(operation, pool_id)? {
                return Err(EmbeddedError::invalid(
                    "operation page cursor does not match the pool filter",
                ));
            }
            operation.publication_sequence
        } else {
            0
        };
        // Registry retention is the sole bound; no unbounded secondary lifecycle index is created.
        // 注册表保留上限是唯一边界；不创建无界的第二生命周期索引。
        let mut records = Vec::new();
        for operation in state.records.values() {
            if operation.publication_sequence > after && matches_pool(operation, pool_id)? {
                records.push(operation);
            }
        }
        records.sort_unstable_by_key(|operation| operation.publication_sequence);
        let has_more = records.len() > limit;
        records.truncate(limit);
        let operation_ids = records
            .into_iter()
            .map(|operation| operation.id.clone())
            .collect::<Vec<_>>();
        let after_operation_id = operation_ids
            .last()
            .cloned()
            .or_else(|| after_operation_id.map(str::to_owned));
        Ok(OperationPage {
            operation_ids,
            after_operation_id,
            has_more,
        })
    }
}

/// Compare optional exact pool authority against the original immutable context, including forgotten pools.
/// 将可选精确池权威与原始不可变上下文比较，包含已遗忘的池。
/// Return true for an unfiltered query and propagate observation failures explicitly.
/// 未过滤查询返回真，并显式传播观测失败。
fn matches_pool(operation: &Operation, pool_id: Option<&str>) -> EmbeddedResult<bool> {
    let Some(pool_id) = pool_id else {
        return Ok(true);
    };
    Ok(
        matches!(&operation.lock()?.context, OperationContext::Module(context) if context.pool_id == pool_id),
    )
}
