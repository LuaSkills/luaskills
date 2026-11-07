use super::{EmbeddedError, EmbeddedErrorCode, EmbeddedResult};
use crate::runtime::json_limits::{MAX_DECODABLE_JSON_DEPTH, nesting_error};
use serde::Serialize;
use serde_json::ser::Formatter;
use std::cell::Cell;
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

/// Compact JSON formatter that also stops serialization once containers nest deeper than `limit`.
/// 紧凑 JSON 格式化器，并在容器嵌套超过 `limit` 层时停止序列化。
struct DepthLimitedFormatter<'a> {
    /// Containers currently open around the next token.
    /// 下一个记号外层当前已打开的容器数。
    depth: usize,
    /// Deepest nesting a later reader can decode.
    /// 之后的读取方能够解码的最大嵌套层数。
    limit: usize,
    /// Set when serialization stopped because of nesting rather than bytes.
    /// 当序列化因嵌套而非字节数停止时置位。
    exceeded: &'a Cell<bool>,
}

impl DepthLimitedFormatter<'_> {
    /// Open one container, returning an I/O error instead of exceeding the nesting limit.
    /// 打开一层容器；超过嵌套上限时返回 I/O 错误。
    fn open(&mut self) -> io::Result<()> {
        if self.depth >= self.limit {
            self.exceeded.set(true);
            return Err(io::Error::other("JSON nesting limit exceeded"));
        }
        self.depth += 1;
        Ok(())
    }
}

impl Formatter for DepthLimitedFormatter<'_> {
    /// Count one array level, then write the same bytes as the compact formatter.
    /// 计入一层数组，然后写出与紧凑格式化器相同的字节。
    fn begin_array<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.open()?;
        writer.write_all(b"[")
    }

    /// Close one array level.
    /// 关闭一层数组。
    fn end_array<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.depth -= 1;
        writer.write_all(b"]")
    }

    /// Count one object level, then write the same bytes as the compact formatter.
    /// 计入一层对象，然后写出与紧凑格式化器相同的字节。
    fn begin_object<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.open()?;
        writer.write_all(b"{")
    }

    /// Close one object level.
    /// 关闭一层对象。
    fn end_object<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.depth -= 1;
        writer.write_all(b"}")
    }
}

/// Return serialized `value` size within `limit`, rejecting oversized values without a full copy.
/// 返回 `limit` 内的序列化 `value` 大小，不完整复制就拒绝超大值。
///
/// Values nested deeper than `MAX_DECODABLE_JSON_DEPTH` are rejected too: serde_json would write
/// them, but no reader could decode them again, so persisted history containing one is unreadable.
/// 嵌套深于 `MAX_DECODABLE_JSON_DEPTH` 的值同样会被拒绝：serde_json 能写出它们，
/// 却没有读取方能再解码，包含这种值的持久历史将无法读取。
///
/// Depth is counted through formatter hooks, which `serde_json::value::RawValue` fragments bypass.
/// This crate does not enable `raw_value`; a value that embeds one must check its depth separately.
/// 层数通过格式化器钩子计算，而 `serde_json::value::RawValue` 片段会绕过这些钩子。
/// 本 crate 未启用 `raw_value`；若有值内嵌 RawValue，必须另行检查其层数。
pub fn json_size(value: &impl Serialize, limit: usize) -> EmbeddedResult<usize> {
    // The sink is the only authority for cumulative serialized bytes.
    // 此接收器是累计序列化字节数的唯一权威。
    let mut counter = LimitedJsonCounter { bytes: 0, limit };
    // Distinguishes a nesting stop from a byte-limit stop; both surface as I/O errors.
    // 区分因嵌套停止与因字节上限停止；二者都以 I/O 错误呈现。
    let exceeded = Cell::new(false);
    let formatter = DepthLimitedFormatter {
        depth: 0,
        limit: MAX_DECODABLE_JSON_DEPTH,
        exceeded: &exceeded,
    };
    let mut serializer = serde_json::Serializer::with_formatter(&mut counter, formatter);
    value.serialize(&mut serializer).map_err(|error| {
        if exceeded.get() {
            EmbeddedError::new(
                EmbeddedErrorCode::CapacityExceeded,
                nesting_error(MAX_DECODABLE_JSON_DEPTH),
            )
        } else if error.is_io() {
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
