use super::{OperationJournalConfig, capacity, corrupt};
use crate::runtime::embedded::{EmbeddedError, EmbeddedErrorCode, EmbeddedResult};
use rusqlite::{Connection, OpenFlags, TransactionBehavior, limits::Limit};
use std::io::Read;
use std::path::Path;
use std::time::Duration;

/// Private SQLite file-format marker, independent of the FFI and package versions.
/// 私有 SQLite 文件格式标记，独立于 FFI 及软件包版本。
const APPLICATION_ID: i64 = 0x4c53_4f4a;
/// Third journal schema requires explicit operation context even before the first host effect; old files stay untouched.
/// 第三版日志结构要求首次宿主副作用前也有明确操作上下文；旧文件保持原字节。
const SCHEMA_VERSION: i64 = 3;
/// Single authority for the file-format page size and database-cap rounding.
/// 文件格式页大小及数据库上限取整的唯一权威。
const PAGE_BYTES: u64 = 4096;
/// SQLite row headers, signed revision and SHA-256 digest fit below this fixed allowance.
/// SQLite 行头、有符号修订号及 SHA-256 摘要均小于此固定余量。
const ROW_OVERHEAD: usize = 128;
/// One table without secondary indexes; schema text is checked before any business mutation.
/// 无辅助索引的单表；任何业务变更前均检查结构文本。
const SCHEMA: &str = "CREATE TABLE operations(runtime_id TEXT NOT NULL, operation_id TEXT NOT NULL, revision INTEGER NOT NULL CHECK(revision > 0), document BLOB NOT NULL, digest BLOB NOT NULL CHECK(length(digest) = 32), PRIMARY KEY(runtime_id, operation_id)) STRICT, WITHOUT ROWID";

/// Open and configure exact `path` with verified durability, ownership and capacity settings.
/// 使用已验证的持久性、所有权及容量设置打开精确 `path`。
pub(super) fn open(path: &Path, config: OperationJournalConfig) -> EmbeddedResult<Connection> {
    if !path.is_absolute()
        || path.file_name().is_none()
        || config.max_records == 0
        || config.max_record_bytes == 0
        || config.max_records > i64::MAX as usize
    {
        return Err(EmbeddedError::invalid(
            "operation history requires an absolute file path and positive bounded limits",
        ));
    }
    // Row storage repeats identities outside the JSON; each is bounded by the whole JSON budget.
    // 行存储在 JSON 外重复身份；每个身份均受完整 JSON 预算约束。
    let row_limit = config
        .max_record_bytes
        .checked_mul(3)
        .and_then(|value| value.checked_add(ROW_OVERHEAD))
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(capacity)?;
    let row_limit = row_limit.max(PAGE_BYTES as i32);
    let max_pages = config.max_database_bytes / PAGE_BYTES;
    if max_pages < 2 || max_pages > i32::MAX as u64 {
        return Err(capacity());
    }
    let parent = path
        .parent()
        .ok_or_else(|| EmbeddedError::invalid("operation history parent is missing"))?;
    if !parent.is_dir() {
        return Err(EmbeddedError::invalid(
            "operation history parent directory must already exist",
        ));
    }
    let existing = match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(EmbeddedError::invalid(
                    "operation history path must be a regular file",
                ));
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if metadata.file_attributes()
                    & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
                    != 0
                {
                    return Err(EmbeddedError::invalid(
                        "operation history path must not be a reparse point",
                    ));
                }
            }
            if metadata.len() > max_pages * PAGE_BYTES {
                return Err(capacity());
            }
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => {
            return Err(EmbeddedError::new(
                EmbeddedErrorCode::Internal,
                "operation history metadata cannot be read",
            ));
        }
    };
    let shared_flags = OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    if existing {
        validate_header(path)?;
    }
    let flags = shared_flags
        | OpenFlags::SQLITE_OPEN_READ_WRITE
        | if existing {
            OpenFlags::empty()
        } else {
            OpenFlags::SQLITE_OPEN_CREATE
        };
    let mut connection = Connection::open_with_flags(path, flags).map_err(error)?;
    connection.busy_timeout(Duration::ZERO).map_err(error)?;
    connection
        .set_limit(Limit::SQLITE_LIMIT_LENGTH, row_limit)
        .map_err(error)?;
    if connection
        .limit(Limit::SQLITE_LIMIT_LENGTH)
        .map_err(error)?
        != row_limit
    {
        return Err(capacity());
    }
    connection
        .pragma_update(None, "trusted_schema", false)
        .map_err(error)?;
    verify_number(&connection, "trusted_schema", 0)?;
    if existing {
        validate_identity(&connection)?;
    }
    connection
        .pragma_update(None, "page_size", PAGE_BYTES as i64)
        .map_err(error)?;
    verify_number(&connection, "page_size", PAGE_BYTES as i64)?;
    connection
        .pragma_update(None, "max_page_count", max_pages as i64)
        .map_err(error)?;
    verify_number(&connection, "max_page_count", max_pages as i64)?;
    let mode: String = connection
        .pragma_update_and_check(None, "journal_mode", "DELETE", |row| row.get(0))
        .map_err(error)?;
    if mode != "delete" {
        return Err(EmbeddedError::new(
            EmbeddedErrorCode::Unsupported,
            "operation history requires SQLite DELETE journal mode",
        ));
    }
    connection
        .pragma_update(None, "synchronous", "EXTRA")
        .map_err(error)?;
    verify_number(&connection, "synchronous", 3)?;
    // Exclusive mode may retain a rollback file after commit; truncate its inactive storage.
    // 独占模式可能在提交后保留回滚文件；截断其非活动存储。
    connection
        .pragma_update(None, "journal_size_limit", 0)
        .map_err(error)?;
    verify_number(&connection, "journal_size_limit", 0)?;
    // macOS uses its stronger full-sync primitive when the platform supports it.
    // 平台支持时，macOS 使用其更强的完整同步原语。
    connection
        .pragma_update(None, "fullfsync", true)
        .map_err(error)?;
    verify_number(&connection, "fullfsync", 1)?;
    let mode: String = connection
        .pragma_update_and_check(None, "locking_mode", "EXCLUSIVE", |row| row.get(0))
        .map_err(error)?;
    if mode != "exclusive" {
        return Err(EmbeddedError::new(
            EmbeddedErrorCode::Unsupported,
            "operation history requires exclusive SQLite ownership",
        ));
    }
    // Acquire the actual OS lock now; EXCLUSIVE mode alone does not acquire it.
    // 现在取得真实操作系统锁；仅设置 EXCLUSIVE 模式不会获取锁。
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Exclusive)
        .map_err(error)?;
    let application_id: i64 = transaction
        .pragma_query_value(None, "application_id", |row| row.get(0))
        .map_err(error)?;
    let version: i64 = transaction
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(error)?;
    let tables: i64 = transaction
        .query_row("SELECT count(*) FROM sqlite_schema", [], |row| row.get(0))
        .map_err(error)?;
    if !existing && application_id == 0 && version == 0 && tables == 0 {
        transaction.execute_batch(SCHEMA).map_err(error)?;
        transaction
            .pragma_update(None, "application_id", APPLICATION_ID)
            .map_err(error)?;
        transaction
            .pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(error)?;
    } else {
        validate_identity(&transaction)?;
    }
    transaction.commit().map_err(error)?;
    validate_identity(&connection)?;
    let integrity: String = connection
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .map_err(error)?;
    if integrity != "ok" {
        return Err(corrupt());
    }
    Ok(connection)
}

/// Read SQLite's fixed header before opening an existing database for possible hot-journal recovery.
/// 打开已有数据库以进行可能的热日志恢复前，读取 SQLite 固定文件头。
/// Format offsets come from SQLite's file-format specification; business writes never change them.
/// 格式偏移来自 SQLite 文件格式规范；业务写入绝不变更这些字段。
fn validate_header(path: &Path) -> EmbeddedResult<()> {
    // Reading through a read-only SQLite connection cannot recover an uncommitted hot journal.
    // 通过只读 SQLite 连接读取无法恢复未提交的热日志。
    let mut header = [0u8; 100];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut header))
        .map_err(|_| corrupt())?;
    let version = u32::from_be_bytes(header[60..64].try_into().map_err(|_| corrupt())?);
    let application_id = u32::from_be_bytes(header[68..72].try_into().map_err(|_| corrupt())?);
    if &header[..16] != b"SQLite format 3\0"
        || i64::from(version) != SCHEMA_VERSION
        || i64::from(application_id) != APPLICATION_ID
    {
        return Err(EmbeddedError::new(
            EmbeddedErrorCode::Unsupported,
            "operation history identity or schema version is unsupported",
        ));
    }
    Ok(())
}

/// Check exact application, version and schema identity, returning an explicit unsupported-format error.
/// 检查精确应用、版本及结构身份，返回明确的不支持格式错误。
fn validate_identity(connection: &Connection) -> EmbeddedResult<()> {
    let application_id: i64 = connection
        .pragma_query_value(None, "application_id", |row| row.get(0))
        .map_err(error)?;
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(error)?;
    if application_id != APPLICATION_ID || version != SCHEMA_VERSION {
        return Err(EmbeddedError::new(
            EmbeddedErrorCode::Unsupported,
            "operation history identity or schema version is unsupported",
        ));
    }
    let count: i64 = connection
        .query_row("SELECT count(*) FROM sqlite_schema", [], |row| row.get(0))
        .map_err(error)?;
    if count != 1 {
        return Err(corrupt());
    }
    let schema: String = connection
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE type='table' AND name='operations'",
            [],
            |row| row.get(0),
        )
        .map_err(error)?;
    if schema != SCHEMA {
        return Err(corrupt());
    }
    Ok(())
}

/// Read back one numeric pragma and reject a silently ignored or clamped requested setting.
/// 回读单个数字 pragma，拒绝被静默忽略或截断的请求设置。
fn verify_number(connection: &Connection, name: &str, expected: i64) -> EmbeddedResult<()> {
    let actual: i64 = connection
        .pragma_query_value(None, name, |row| row.get(0))
        .map_err(error)?;
    if actual != expected {
        return Err(EmbeddedError::new(
            EmbeddedErrorCode::Unsupported,
            "operation history SQLite settings do not match required limits",
        ));
    }
    Ok(())
}

/// Classify database failures without echoing paths, SQL parameters or plugin-provided values.
/// 分类数据库故障，不回显路径、SQL 参数或插件提供值。
pub(super) fn error(failure: rusqlite::Error) -> EmbeddedError {
    let code = match failure.sqlite_error_code() {
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => {
            EmbeddedErrorCode::Busy
        }
        Some(rusqlite::ErrorCode::DiskFull | rusqlite::ErrorCode::TooBig) => {
            EmbeddedErrorCode::CapacityExceeded
        }
        _ => EmbeddedErrorCode::Internal,
    };
    EmbeddedError::new(code, "operation history database operation failed")
}
