use super::{EmbeddedError, EmbeddedErrorCode, EmbeddedResult};
use serde::Serialize;
use std::io::{self, Write};

/// Count-only JSON sink avoids allocating a second complete serialization merely to enforce limits.
/// 仅计数 JSON 接收器避免仅为检查上限而分配第二份完整序列化结果。
struct LimitedJsonCounter {
    /// Accepted bytes so far; checked before each increment.
    /// 已接纳字节数；每次增加前检查。
    bytes: usize,
    /// Exact configured byte boundary.
    /// 精确配置的字节边界。
    limit: usize,
}

impl Write for LimitedJsonCounter {
    /// Count `buffer` without copying it, returning an I/O limit error before overflow.
    /// 对 `buffer` 计数且不复制，在溢出前返回 I/O 上限错误。
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.len() > self.limit.saturating_sub(self.bytes) {
            return Err(io::Error::other("JSON byte limit exceeded"));
        }
        self.bytes += buffer.len();
        Ok(buffer.len())
    }

    /// A counter has no buffered output to flush.
    /// 计数器没有需要刷新的缓冲输出。
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Return serialized `value` size within `limit`, rejecting oversized values without a full copy.
/// 返回 `limit` 内的序列化 `value` 大小，不完整复制就拒绝超大值。
pub fn json_size(value: &impl Serialize, limit: usize) -> EmbeddedResult<usize> {
    // The sink is the only authority for cumulative serialized bytes.
    // 此接收器是累计序列化字节数的唯一权威。
    let mut counter = LimitedJsonCounter { bytes: 0, limit };
    serde_json::to_writer(&mut counter, value).map_err(|error| {
        if error.is_io() {
            EmbeddedError::new(
                EmbeddedErrorCode::CapacityExceeded,
                "JSON value exceeds its configured byte limit",
            )
        } else {
            EmbeddedError::new(
                EmbeddedErrorCode::InvalidArgument,
                "JSON serialization failed",
            )
        }
    })?;
    Ok(counter.bytes)
}

/// Byte limits include UTF-8 encoding and JSON escapes rather than character counts.
/// 字节限制包含 UTF-8 编码与 JSON 转义，而非字符数。
#[test]
fn embedded_value_size_matches_wire_bytes_at_exact_boundary() {
    // Include multibyte text and an embedded zero byte requiring a JSON escape.
    // 包含多字节文本与需要 JSON 转义的嵌入零字节。
    let value = serde_json::json!({"text":"中文\0🦀"});
    let expected = serde_json::to_vec(&value).unwrap().len();
    assert_eq!(json_size(&value, expected).unwrap(), expected);
    assert_eq!(
        json_size(&value, expected - 1).unwrap_err().code,
        EmbeddedErrorCode::CapacityExceeded
    );
}
