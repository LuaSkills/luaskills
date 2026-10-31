//! Local SQLite host owned exclusively by the standalone debugger.
//! 仅由独立调试工具拥有的本地 SQLite 宿主。

use luaskills::{RuntimeDatabaseProviderCallbacks, RuntimeSqliteProviderAction};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::{Connection, params_from_iter, types::Value as SqlValue, types::ValueRef};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// One SQL statement or parameter-free script accepted by this host.
/// 此宿主接受的一条 SQL 语句或无参数脚本。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SqlInput {
    /// SQL text; scripts are allowed only by execute_script without parameters.
    /// SQL 文本；仅无参数的 execute_script 允许多语句脚本。
    sql: String,
    /// Positional parameters; nested byte arrays represent SQLite blobs.
    /// 位置参数；嵌套字节数组表示 SQLite 二进制值。
    #[serde(default)]
    params: Vec<Value>,
}

/// An ordered group of statements committed atomically by execute_batch.
/// 由 execute_batch 原子提交的有序语句组。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchInput {
    /// Each entry is one parameterized statement; an empty batch changes nothing.
    /// 每个条目是一条带参数语句；空批次不修改任何数据。
    statements: Vec<SqlInput>,
}

/// Parsed actions supported by the debugger's documented SQLite contract.
/// 调试工具已文档化 SQLite 契约支持的已解析动作。
enum SqlOperation {
    /// Execute SQL text, allowing an unparameterized multi-statement script.
    /// 执行 SQL 文本，允许不带参数的多语句脚本。
    Script(SqlInput),
    /// Execute an atomic ordered transaction.
    /// 执行原子有序事务。
    Batch(BatchInput),
    /// Return named JSON rows from one statement.
    /// 从一条语句返回具名 JSON 行。
    Query(SqlInput),
}

/// Create engine-owned callbacks with a fresh connection cache; takes no arguments.
/// 使用全新连接缓存创建引擎拥有的回调；无参数。
/// Return registrations that keep connections alive until the engine releases its snapshot.
/// 返回注册集合，使连接保持存活直到引擎释放其快照。
pub(super) fn database_callbacks() -> RuntimeDatabaseProviderCallbacks {
    // Serialize operations per debugger engine, preserving explicit SQL transaction state across calls.
    // 按调试引擎串行执行操作，跨调用保留显式 SQL 事务状态。
    let connections = Mutex::new(HashMap::<PathBuf, Connection>::new());
    RuntimeDatabaseProviderCallbacks::default().with_sqlite(Arc::new(move |request| {
        // Parse before opening storage so unsupported operations cannot create empty databases.
        // 在打开存储前解析，使不支持的操作不会创建空数据库。
        let operation = match request.action {
            RuntimeSqliteProviderAction::ExecuteScript => SqlOperation::Script(
                serde_json::from_value(request.input.clone()).map_err(|error| error.to_string())?,
            ),
            RuntimeSqliteProviderAction::ExecuteBatch => SqlOperation::Batch(
                serde_json::from_value(request.input.clone()).map_err(|error| error.to_string())?,
            ),
            RuntimeSqliteProviderAction::QueryJson => SqlOperation::Query(
                serde_json::from_value(request.input.clone()).map_err(|error| error.to_string())?,
            ),
            _ => {
                return Err(format!(
                    "luaskills-debug SQLite does not support {:?}; use execute_script, execute_batch or query_json with SQLite SQL",
                    request.action
                ));
            }
        };
        // Only the library-owned outer binding chooses storage; input contains no path option.
        // 仅库拥有的外层绑定决定存储位置；输入不包含路径选项。
        let path = PathBuf::from(&request.binding.default_database_path);
        // Poisoning is reported rather than silently reusing uncertain host transaction state.
        // 锁中毒直接报告，避免静默复用状态不确定的宿主事务。
        let mut connections = connections
            .lock()
            .map_err(|_| "debug SQLite connection lock was poisoned".to_string())?;
        // Cache exactly one connection for each binding path for this engine lifetime.
        // 在此引擎生命周期内，为每个绑定路径缓存精确的一条连接。
        let connection = match connections.entry(path) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let connection = open_connection(entry.key())?;
                entry.insert(connection)
            }
        };
        match operation {
            SqlOperation::Script(input) => execute_script(connection, &input),
            SqlOperation::Batch(input) => execute_batch(connection, &input),
            SqlOperation::Query(input) => query_json(connection, &input),
        }
    }))
}

/// Open the exact binding path, creating its parent directory and preserving existing data.
/// 打开精确绑定路径，创建其父目录并保留既有数据。
/// Return a connection with foreign keys enabled and a bounded five-second busy wait.
/// 返回启用外键且繁忙等待限制为五秒的连接。
fn open_connection(path: &Path) -> Result<Connection, String> {
    // Runtime bindings identify files, so a missing parent is an explicit contract violation.
    // 运行时绑定指向文件，因此缺少父目录属于明确的契约违规。
    let parent = path
        .parent()
        .ok_or_else(|| "debug SQLite binding has no parent directory".to_string())?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    // Reopening never truncates an existing SQLite database or replaces its schema.
    // 重新打开绝不截断既有 SQLite 数据库，也不替换其结构。
    let connection = Connection::open(path).map_err(|error| error.to_string())?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(|error| error.to_string())?;
    connection
        .pragma_update(None, "foreign_keys", true)
        .map_err(|error| error.to_string())?;
    Ok(connection)
}

/// Convert one JSON parameter to an exact SQLite value, rejecting objects and integer overflow.
/// 将一个 JSON 参数转换为精确 SQLite 值，拒绝对象及整数溢出。
/// Return the owned value, with byte arrays representing blobs and booleans encoded as 0/1.
/// 返回自有值，字节数组表示二进制，布尔值编码为 0/1。
fn sql_value(value: &Value) -> Result<SqlValue, String> {
    match value {
        Value::Null => Ok(SqlValue::Null),
        Value::Bool(value) => Ok(SqlValue::Integer(i64::from(*value))),
        Value::Number(value) if value.is_i64() => {
            Ok(SqlValue::Integer(value.as_i64().expect("checked integer")))
        }
        Value::Number(value) if value.is_u64() => {
            Err("SQLite parameters must fit in a signed 64-bit integer".to_string())
        }
        Value::Number(value) => Ok(SqlValue::Real(value.as_f64().expect("JSON float"))),
        Value::String(value) => Ok(SqlValue::Text(value.clone())),
        Value::Array(_) => serde_json::from_value::<Vec<u8>>(value.clone())
            .map(SqlValue::Blob)
            .map_err(|error| format!("SQLite blob must be a byte array: {error}")),
        Value::Object(_) => Err("SQLite parameters cannot be JSON objects".to_string()),
    }
}

/// Validate SQL text and convert input parameters without executing SQL.
/// 验证 SQL 文本并转换输入参数，不执行 SQL。
/// Return ordered owned values or an explicit input error.
/// 返回有序自有值或明确的输入错误。
fn parameters(input: &SqlInput) -> Result<Vec<SqlValue>, String> {
    if input.sql.trim().is_empty() {
        return Err("SQLite SQL must not be empty".to_string());
    }
    input.params.iter().map(sql_value).collect()
}

/// Execute input on connection, using script semantics only when parameters are absent or empty.
/// 在 connection 上执行 input，仅在参数缺省或为空时采用脚本语义。
/// Return total affected rows and the connection's last inserted row id; script transactions are explicit.
/// 返回总影响行数和连接的最近插入行号；脚本事务由 SQL 显式控制。
fn execute_script(connection: &Connection, input: &SqlInput) -> Result<Value, String> {
    // Validate values before any statement can change data.
    // 在任何语句可能修改数据之前验证值。
    let values = parameters(input)?;
    // Total changes account for all statements in a script, including trigger effects.
    // 总变更数覆盖脚本中所有语句，包括触发器效果。
    let before = connection.total_changes();
    if values.is_empty() {
        // Prepare each statement with SQLite's parser; missing bindings must not silently become NULL.
        // 使用 SQLite 解析器逐条准备语句；缺少绑定时不得静默变成 NULL。
        let mut script = rusqlite::Batch::new(connection, &input.sql);
        while let Some(mut statement) =
            rusqlite::fallible_iterator::FallibleIterator::next(&mut script)
                .map_err(|error| error.to_string())?
        {
            // An explicit empty parameter list verifies arity before any statement starts.
            // 显式空参数列表在语句开始之前验证参数数量。
            let mut cursor = statement.query([]).map_err(|error| error.to_string())?;
            while cursor.next().map_err(|error| error.to_string())?.is_some() {}
        }
    } else {
        connection
            .execute(&input.sql, params_from_iter(values.iter()))
            .map_err(|error| error.to_string())?;
    }
    Ok(
        json!({"changes": connection.total_changes() - before, "last_insert_rowid": connection.last_insert_rowid()}),
    )
}

/// Execute every statement in input inside one transaction on connection.
/// 在 connection 的单个事务中执行 input 的全部语句。
/// Return committed affected rows and the last row id; any failure rolls back the batch.
/// 返回已提交的影响行数和最近行号；任何失败都会回滚整个批次。
fn execute_batch(connection: &mut Connection, input: &BatchInput) -> Result<Value, String> {
    // Validate the complete batch before acquiring a transaction or changing data.
    // 在开启事务或修改数据之前验证完整批次。
    let values = input
        .statements
        .iter()
        .map(parameters)
        .collect::<Result<Vec<_>, _>>()?;
    // Transaction ownership guarantees rollback if execution or commit fails.
    // 事务所有权确保执行或提交失败时回滚。
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    // User SQL must not commit the host-owned transaction halfway through a batch.
    // 用户 SQL 不得在批次执行到一半时提交宿主拥有的事务。
    transaction
        .authorizer(Some(|context: AuthContext<'_>| match context.action {
            AuthAction::Transaction { .. } | AuthAction::Savepoint { .. } => Authorization::Deny,
            _ => Authorization::Allow,
        }))
        .map_err(|error| error.to_string())?;
    // Preserve the error until the authorizer is cleared, allowing the owner's rollback to run.
    // 保留错误直到解除授权器，使所有者的回滚可以执行。
    let result =
        input
            .statements
            .iter()
            .zip(&values)
            .try_fold(0usize, |changes, (statement, values)| {
                transaction
                    .execute(&statement.sql, params_from_iter(values.iter()))
                    .map(|updated| changes + updated)
            });
    transaction
        .authorizer(None::<fn(AuthContext<'_>) -> Authorization>)
        .map_err(|error| error.to_string())?;
    // Only an entirely successful batch may reach the commit below.
    // 仅全部成功的批次才能到达下方提交。
    let changes = result.map_err(|error| error.to_string())?;
    transaction.commit().map_err(|error| error.to_string())?;
    Ok(json!({"changes": changes, "last_insert_rowid": connection.last_insert_rowid()}))
}

/// Convert one borrowed SQLite cell to JSON without losing binary or integer data.
/// 将一个借用 SQLite 单元格转换为 JSON，不丢失二进制或整数数据。
/// Return an owned JSON value; invalid UTF-8 and non-finite numbers are explicit errors.
/// 返回自有 JSON 值；无效 UTF-8 与非有限数字明确报错。
fn json_cell(value: ValueRef<'_>) -> Result<Value, String> {
    match value {
        ValueRef::Null => Ok(Value::Null),
        ValueRef::Integer(value) => Ok(json!(value)),
        ValueRef::Real(value) => serde_json::Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| "SQLite returned a non-finite number".to_string()),
        ValueRef::Text(value) => std::str::from_utf8(value)
            .map(|value| Value::String(value.to_string()))
            .map_err(|error| format!("SQLite text is not UTF-8: {error}")),
        ValueRef::Blob(value) => Ok(json!(value)),
    }
}

/// Query one parameterized statement on connection using input.
/// 使用 input 在 connection 上查询一条带参数语句。
/// Return columns, named rows and row_count; duplicate column names must be explicitly aliased.
/// 返回列名、具名行和 row_count；重复列名必须显式设置别名。
fn query_json(connection: &Connection, input: &SqlInput) -> Result<Value, String> {
    // Keep parameter allocations alive throughout cursor iteration.
    // 在游标遍历期间保持参数分配存活。
    let values = parameters(input)?;
    // Rusqlite rejects extra SQL statements instead of silently discarding a suffix.
    // Rusqlite 拒绝额外 SQL 语句，不静默丢弃后缀。
    let mut statement = connection
        .prepare(&input.sql)
        .map_err(|error| error.to_string())?;
    // Owned column names remain valid while the statement is mutably borrowed by its cursor.
    // 语句被游标可变借用期间，自有列名仍然有效。
    let columns = statement
        .column_names()
        .iter()
        .map(|name| name.to_string())
        .collect::<Vec<_>>();
    if columns.is_empty() {
        return Err("query_json requires a statement with result columns".to_string());
    }
    if columns.iter().collect::<HashSet<_>>().len() != columns.len() {
        return Err("query_json requires unique column names; use SQL AS aliases".to_string());
    }
    // SQLite owns each borrowed row only until the cursor advances.
    // SQLite 仅在游标前进之前拥有当前借用行。
    let mut cursor = statement
        .query(params_from_iter(values.iter()))
        .map_err(|error| error.to_string())?;
    // The returned JSON row set owns all converted cells.
    // 返回的 JSON 行集拥有全部转换后的单元格。
    let mut rows = Vec::new();
    while let Some(row) = cursor.next().map_err(|error| error.to_string())? {
        // Unique names prevent silently overwriting data from a second column.
        // 唯一列名防止静默覆盖另一列的数据。
        let mut object = Map::new();
        for (index, name) in columns.iter().enumerate() {
            object.insert(
                name.clone(),
                json_cell(row.get_ref(index).map_err(|error| error.to_string())?)?,
            );
        }
        rows.push(Value::Object(object));
    }
    Ok(json!({"columns": columns, "row_count": rows.len(), "rows": rows}))
}

#[cfg(test)]
mod tests;
