//! Explicit storage recovery and exact original-checkpoint reconciliation.
//! 显式存储恢复与精确原检查点对账。

use super::*;

impl JournalState {
    /// Borrow the connection only after the caller has checked this state's healthy invariant.
    /// 仅在调用方已经检查此状态的健康不变量后借用连接。
    pub(super) fn connection(&self) -> &Connection {
        self.connection
            .as_ref()
            .expect("healthy journal retains its exclusive connection")
    }
}

impl OperationJournal {
    /// Recover this exact journal after an uncertain transaction; return false when no recovery is needed.
    /// 不确定事务后恢复此精确日志；无需恢复时返回假。
    /// This blocking host call closes the old connection, reopens existing storage, and validates all records.
    /// 此阻塞宿主调用关闭旧连接、重新打开既有存储并校验全部记录。
    /// Failure keeps reads and writes blocked; success never retries a checkpoint or executes plugin code.
    /// 失败继续阻止读写；成功绝不重试检查点或执行插件代码。
    pub fn recover_storage(&self) -> EmbeddedResult<bool> {
        // Recovery serializes only with actual journal I/O, never with scheduler or callback locks.
        // 恢复仅与真实日志 I/O 串行化，绝不持有调度器或回调锁。
        let mut state = self.state.lock().map_err(|_| {
            EmbeddedError::new(
                EmbeddedErrorCode::Internal,
                "operation history lock is poisoned",
            )
        })?;
        if state.failure.is_none() {
            return Ok(false);
        }
        if let Some(connection) = state.connection.take()
            && let Err((connection, failure)) = connection.close()
        {
            state.connection = Some(connection);
            return Err(storage::error(failure));
        }
        // The failed state remains authoritative throughout open and validation, including an early return.
        // 在打开及校验全程，包括提前返回时，失败状态始终保持权威。
        let connection = storage::reopen(&self.path, self.config)?;
        self.validate_contents(&connection)?;
        state.connection = Some(connection);
        state.failure = None;
        Ok(true)
    }

    /// Validate all documents on `connection` within the journal's immutable count and byte limits.
    /// 在日志不可变数量及字节上限内校验 `connection` 上的全部文档。
    /// Return success only when every stored identity, digest and context passes the normal decoder.
    /// 仅当每个存储身份、摘要及上下文均通过普通解码器时返回成功。
    pub(super) fn validate_contents(&self, connection: &Connection) -> EmbeddedResult<()> {
        // Check the actual retained count before streaming individual bounded records.
        // 在逐条流式读取有界记录前检查真实保留数量。
        let count: i64 = connection
            .query_row("SELECT count(*) FROM operations", [], |row| row.get(0))
            .map_err(storage::error)?;
        if count < 0 || count as u64 > self.config.max_records as u64 {
            return Err(capacity());
        }
        // No whole-database result is allocated during startup or recovery.
        // 启动或恢复时均不分配整个数据库的结果。
        let mut statement = connection
            .prepare("SELECT runtime_id, operation_id, revision, document, digest FROM operations")
            .map_err(storage::error)?;
        // The cursor borrows the sole connection while validating each row's own bytes.
        // 游标借用唯一连接，并校验每行自身字节。
        let mut rows = statement.query([]).map_err(storage::error)?;
        while let Some(row) = rows.next().map_err(storage::error)? {
            self.decode(row)?;
        }
        Ok(())
    }

    /// Persist the immutable scheduler candidate or acknowledge its exact already-committed successor.
    /// 持久化不可变调度候选，或确认其精确的已提交后继。
    /// `runtime_id`, `previous` and `snapshot` come from the original owner; this never replays business work.
    /// `runtime_id`、`previous` 及 `snapshot` 来自原所有者；此方法绝不重放业务工作。
    pub(in crate::runtime::embedded) fn checkpoint(
        &self,
        runtime_id: &str,
        previous: Option<u64>,
        snapshot: &OperationSnapshot,
    ) -> EmbeddedResult<JournalOperation> {
        self.write_checkpoint(runtime_id, previous, snapshot, true)
    }

    /// Write `snapshot` against `previous` for `runtime_id`; `reconcile` permits only an identical successor.
    /// 为 `runtime_id` 按 `previous` 写入 `snapshot`；`reconcile` 仅允许相同后继。
    /// Return the acknowledged record; a different value, identity, context or revision remains a conflict.
    /// 返回已确认记录；不同值、身份、上下文或修订仍为冲突。
    pub(super) fn write_checkpoint(
        &self,
        runtime_id: &str,
        previous: Option<u64>,
        snapshot: &OperationSnapshot,
        reconcile: bool,
    ) -> EmbeddedResult<JournalOperation> {
        self.validate_key(runtime_id, &snapshot.operation_id)?;
        validate_callers(runtime_id, snapshot)?;
        json_size(snapshot, self.config.max_record_bytes)?;
        // Absence explicitly denotes insertion; zero is never a valid stored predecessor.
        // 缺失明确表示插入；零绝非有效的存储前驱。
        let revision = match previous {
            None => 1,
            Some(previous) if previous > 0 && previous < i64::MAX as u64 => previous + 1,
            Some(_) => {
                return Err(EmbeddedError::invalid(
                    "operation history revision is exhausted or invalid",
                ));
            }
        };
        // Serialize one exact candidate, preserving missing versus explicit-null results.
        // 序列化单个精确候选，保留缺失结果与显式空值的区别。
        let record = JournalOperation {
            runtime_id: runtime_id.to_owned(),
            revision,
            snapshot: snapshot.clone(),
            reconciliation: None,
        };
        // The same bytes supply both comparison and durable mutation.
        // 同一字节同时用于比较及持久变更。
        let document = self.encode(&record)?;
        self.transaction(|connection| {
            // Read under the transaction so reconciliation cannot race another owner mutation.
            // 在事务内读取，使对账无法与其他所有者变更竞争。
            let current = self.read(connection, runtime_id, &snapshot.operation_id)?;
            if reconcile
                && let Some(current) = &current
                && current.revision == revision
                && self.encode(current)? == document
            {
                return Ok(());
            }
            match previous {
                None => {
                    if current.is_some() {
                        return Err(EmbeddedError::new(EmbeddedErrorCode::AlreadyCompleted, "operation history identity already exists"));
                    }
                    // Retained count is checked atomically with the actual insertion.
                    // 保留数量随真实插入原子检查。
                    let count: i64 = connection.query_row("SELECT count(*) FROM operations", [], |row| row.get(0)).map_err(storage::error)?;
                    if count as u64 >= self.config.max_records as u64 { return Err(capacity()); }
                    connection.execute("INSERT INTO operations(runtime_id, operation_id, revision, document, digest) VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![runtime_id, snapshot.operation_id, revision as i64, document, Sha256::digest(&document).as_slice()]).map_err(storage::error)?;
                }
                Some(previous) => {
                    // Missing or advanced evidence cannot be interpreted as permission to recreate an operation.
                    // 缺失或已推进证据不能被解释为重新创建操作的许可。
                    let current = current.ok_or_else(|| EmbeddedError::new(EmbeddedErrorCode::NotFound, "operation history was not found"))?;
                    if current.revision != previous {
                        return Err(EmbeddedError::new(EmbeddedErrorCode::StaleGeneration, "operation history revision changed"));
                    }
                    if current.snapshot.context != snapshot.context {
                        return Err(EmbeddedError::invalid("operation history context is immutable"));
                    }
                    if current.reconciliation.is_some() {
                        return Err(EmbeddedError::new(EmbeddedErrorCode::AlreadyCompleted, "reconciled operation history is immutable"));
                    }
                    connection.execute("UPDATE operations SET revision=?3, document=?4, digest=?5 WHERE runtime_id=?1 AND operation_id=?2",
                        params![runtime_id, snapshot.operation_id, revision as i64, document, Sha256::digest(&document).as_slice()]).map_err(storage::error)?;
                }
            }
            Ok(())
        })?;
        Ok(record)
    }

    /// Lose the next real transaction confirmation after committing or rolling back as `committed` specifies.
    /// 按 `committed` 指定提交或回滚后，丢失下次真实事务确认。
    /// This test-only control exercises SQLite's real wrapper error without simulating stored document bytes.
    /// 此仅测试控制检验 SQLite 真实包装器错误，不模拟存储文档字节。
    #[cfg(test)]
    pub(crate) fn lose_next_confirmation_for_test(&self, committed: bool) {
        self.lock().unwrap().lost_confirmation = Some(committed);
    }
}
