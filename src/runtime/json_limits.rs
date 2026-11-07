//! Nesting limits shared by every boundary where application values become JSON.
//! 应用值转换为 JSON 的所有边界共享的嵌套上限。

use mlua::{Table, Value as LuaValue};
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::ffi::c_void;

/// Deepest container nesting serde_json reads back under its default recursion limit of 128.
/// serde_json 在默认递归上限 128 下能够读回的最大容器嵌套层数。
///
/// Anything written deeper is accepted by the serializer but rejected by every later reader.
/// 超过该层数的内容序列化器仍会接受，但之后的所有读取方都会拒绝。
pub(crate) const MAX_DECODABLE_JSON_DEPTH: usize = 127;

/// Deepest container nesting accepted for one application value crossing a host boundary.
/// 单个应用值跨越宿主边界时允许的最大容器嵌套层数。
///
/// The margin below `MAX_DECODABLE_JSON_DEPTH` leaves room for the envelopes and journal records
/// that wrap a value, so an accepted value can always be written and read back.
/// 与 `MAX_DECODABLE_JSON_DEPTH` 之间的余量留给包裹该值的信封和日志记录，
/// 保证已接受的值一定能写入并读回。
pub(crate) const MAX_JSON_NESTING_DEPTH: usize = 100;

/// Return the error text for a value nested deeper than `limit` containers.
/// 返回嵌套超过 `limit` 层容器时的错误文本。
pub(crate) fn nesting_error(limit: usize) -> String {
    format!("JSON value nesting exceeds {limit} levels")
}

/// Check that JSON `value` nests no deeper than `limit` containers before it is converted for Lua.
/// 在 JSON `value` 被转换给 Lua 之前，检查其容器嵌套不超过 `limit` 层。
///
/// The walk is iterative and visits each node once. Return the nesting error text when exceeded.
/// 遍历采用迭代方式，每个节点只访问一次。超出上限时返回嵌套错误文本。
pub(crate) fn check_json_value_depth(value: &JsonValue, limit: usize) -> Result<(), String> {
    // Pending containers with the depth at which they were reached; the root container is level 1.
    // 待处理的容器及到达它们时的层级；根容器为第 1 层。
    let mut pending: Vec<(&JsonValue, usize)> = vec![(value, 1)];
    while let Some((node, depth)) = pending.pop() {
        if !(node.is_array() || node.is_object()) {
            continue;
        }
        if depth > limit {
            return Err(nesting_error(limit));
        }
        match node {
            JsonValue::Array(items) => pending.extend(items.iter().map(|child| (child, depth + 1))),
            JsonValue::Object(entries) => {
                pending.extend(entries.values().map(|child| (child, depth + 1)))
            }
            _ => {}
        }
    }
    Ok(())
}

/// Check that Lua `value` nests no deeper than `limit` tables before a recursive serializer sees it.
/// 在递归序列化器处理 Lua `value` 之前，检查其表嵌套不超过 `limit` 层。
///
/// The walk is iterative, so a very deep or cyclic table cannot overflow the native stack here.
/// Each table is revisited only when reached at a greater depth, which bounds shared references.
/// 遍历采用迭代方式，因此过深或带环的表不会在这里耗尽原生栈。
/// 每个表只在以更深层级到达时才重新访问，从而限制共享引用带来的重复遍历。
///
/// Return `Ok(())` when every table chain stays within `limit`, or the nesting error text otherwise.
/// 所有表链都在 `limit` 之内时返回 `Ok(())`，否则返回嵌套错误文本。
pub(crate) fn check_lua_json_depth(value: &LuaValue, limit: usize) -> Result<(), String> {
    let LuaValue::Table(root) = value else {
        return Ok(());
    };
    // Deepest level at which each table has been scheduled; keyed by the table's identity.
    // 每个表已被调度的最深层级，以表的身份为键。
    let mut deepest = HashMap::<*const c_void, usize>::new();
    // Pending tables with the container depth at which they were reached.
    // 待处理的表及到达它们时的容器层级。
    let mut pending: Vec<(Table, usize)> = vec![(root.clone(), 1)];
    while let Some((table, depth)) = pending.pop() {
        if depth > limit {
            return Err(nesting_error(limit));
        }
        match deepest.get(&table.to_pointer()) {
            Some(seen) if *seen >= depth => continue,
            _ => {
                deepest.insert(table.to_pointer(), depth);
            }
        }
        // Only values nest: a table used as a key is not a JSON key, and the deserializer rejects it
        // as an invalid key without descending into it, which yields the accurate error.
        // 只有值会嵌套：作为键的表不是合法的 JSON 键，反序列化器会将其作为无效键拒绝而不深入其中，
        // 从而给出准确的错误。
        for pair in table.pairs::<LuaValue, LuaValue>() {
            let (_, child) = pair.map_err(|error| format!("JSON table entry: {error}"))?;
            if let LuaValue::Table(nested) = child {
                pending.push((nested, depth + 1));
            }
        }
    }
    Ok(())
}
