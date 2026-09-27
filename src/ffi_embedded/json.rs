//! Validate raw embedded JSON before typed deserialization can discard ambiguous input evidence.
//! 在类型化反序列化可能丢弃歧义输入证据前校验原始嵌入式 JSON。

use super::EmbeddedFfiStatus;
use serde::de::{Deserialize, Deserializer, Error, MapAccess, SeqAccess, Visitor};
use std::{collections::BTreeSet, fmt};

/// A discard-only JSON visitor that preserves object-member uniqueness checks at every depth.
/// 仅丢弃值的 JSON 访问器，在每个深度保留对象成员唯一性检查。
struct UniqueJson;

impl<'de> Deserialize<'de> for UniqueJson {
    /// Inspect one value from `deserializer`; return no retained payload or a strict JSON error.
    /// 检查 `deserializer` 的一个值；不保留载荷，或返回严格 JSON 错误。
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(Self)
    }
}

impl<'de> Visitor<'de> for UniqueJson {
    type Value = Self;

    /// Describe the accepted input for `formatter`; return the formatting result.
    /// 为 `formatter` 描述接受的输入；返回格式化结果。
    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("JSON with unique object members")
    }

    /// Accept JSON null without retaining data.
    /// 接受 JSON 空值，不保留数据。
    fn visit_unit<E: Error>(self) -> Result<Self, E> {
        Ok(self)
    }

    /// Accept boolean `_value` without retaining data.
    /// 接受布尔 `_value`，不保留数据。
    fn visit_bool<E: Error>(self, _value: bool) -> Result<Self, E> {
        Ok(self)
    }

    /// Accept signed `_value`; exact token range is checked separately before typed decoding.
    /// 接受有符号 `_value`；类型化解码前单独检查精确词元范围。
    fn visit_i64<E: Error>(self, _value: i64) -> Result<Self, E> {
        Ok(self)
    }

    /// Accept unsigned `_value`; exact token range is checked separately before typed decoding.
    /// 接受无符号 `_value`；类型化解码前单独检查精确词元范围。
    fn visit_u64<E: Error>(self, _value: u64) -> Result<Self, E> {
        Ok(self)
    }

    /// Accept finite `value`, returning an error for non-finite numeric input.
    /// 接受有限 `value`，对非有限数值输入返回错误。
    fn visit_f64<E: Error>(self, value: f64) -> Result<Self, E> {
        if value.is_finite() {
            Ok(self)
        } else {
            Err(E::custom("embedded JSON float is not finite"))
        }
    }

    /// Accept `_value` after serde_json has validated UTF-8 and paired surrogate escapes.
    /// 在 serde_json 校验 UTF-8 及成对代理转义后接受 `_value`。
    fn visit_str<E: Error>(self, _value: &str) -> Result<Self, E> {
        Ok(self)
    }

    /// Validate all children of `sequence`, returning no retained collection.
    /// 校验 `sequence` 全部子项，不返回保留集合。
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self, A::Error> {
        while sequence.next_element::<Self>()?.is_some() {}
        Ok(self)
    }

    /// Validate `object` using decoded key identity; return an error before duplicates are overwritten.
    /// 使用解码后键身份校验 `object`；在重复成员被覆盖前返回错误。
    fn visit_map<A: MapAccess<'de>>(self, mut object: A) -> Result<Self, A::Error> {
        // Only keys of currently open objects remain live; the transport already bounds the complete request.
        // 仅保留当前开放对象的键；传输已经限制完整请求大小。
        let mut keys = BTreeSet::new();
        while let Some(key) = object.next_key::<String>()? {
            if !keys.insert(key) {
                return Err(A::Error::custom("duplicate embedded JSON object member"));
            }
            object.next_value::<Self>()?;
        }
        Ok(self)
    }
}

/// Validate bounded `bytes` before mutation; return InvalidArgument for ambiguous or unrepresentable JSON.
/// 在变更前校验有界 `bytes`；对歧义或不可表示 JSON 返回 InvalidArgument。
pub(super) fn validate(bytes: &[u8]) -> Result<(), EmbeddedFfiStatus> {
    // Keep serde_json's existing syntax and recursion limits; do not invent a second JSON grammar.
    // 保留 serde_json 既有语法及递归限制；不另造 JSON 语法。
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    UniqueJson::deserialize(&mut decoder).map_err(|_| EmbeddedFfiStatus::InvalidArgument)?;
    decoder
        .end()
        .map_err(|_| EmbeddedFfiStatus::InvalidArgument)?;

    // serde_json converts overflowing integer tokens to floats; inspect tokens only after syntax is proven valid.
    // serde_json 会将溢出整数字面量转换为浮点；仅在证明语法有效后检查词元。
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                index += 1;
                while bytes[index] != b'"' {
                    if bytes[index] == b'\\' {
                        index += 1;
                    }
                    index += 1;
                }
                index += 1;
            }
            b'-' | b'0'..=b'9' => {
                let start = index;
                while index < bytes.len()
                    && matches!(bytes[index], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
                {
                    index += 1;
                }
                let token = std::str::from_utf8(&bytes[start..index])
                    .map_err(|_| EmbeddedFfiStatus::InvalidArgument)?;
                if !token.contains(['.', 'e', 'E']) {
                    let valid = if token.starts_with('-') {
                        token.parse::<i64>().is_ok()
                    } else {
                        token.parse::<u64>().is_ok()
                    };
                    if !valid {
                        return Err(EmbeddedFfiStatus::InvalidArgument);
                    }
                }
            }
            _ => index += 1,
        }
    }
    Ok(())
}
