//! Regression tests for nesting limits at Lua value boundaries: nested calls and Lua-to-JSON.
//! Lua 值边界嵌套上限的回归测试：嵌套调用与 Lua 到 JSON 的转换。

use super::*;
use crate::runtime::json_limits::{MAX_JSON_NESTING_DEPTH, check_lua_json_depth};
use mlua::LuaSerdeExt;

/// Canonical tool name of the self-recursive fixture skill.
/// 自递归夹具技能的规范工具名。
const RECURSIVE_TOOL: &str = "vulcan-recursive-skill-ping";

/// Lua entry that calls itself through `vulcan.call` until `args.limit` nested levels, or forever.
/// 通过 `vulcan.call` 调用自身的 Lua 入口，直到 `args.limit` 层嵌套，未指定时无限递归。
const RECURSIVE_SOURCE: &str = r#"return function(args)
  local depth = args.depth or 0
  if args.limit ~= nil and depth >= args.limit then
    return tostring(depth)
  end
  if args.catch and depth == 0 then
    local ok, err = pcall(vulcan.call, "vulcan-recursive-skill-ping", { depth = 1 })
    return tostring(ok) .. "|" .. tostring(err)
  end
  return vulcan.call("vulcan-recursive-skill-ping", { depth = depth + 1, limit = args.limit })
end
"#;

/// Build one single-VM engine with skill `skill_id` running `source`; return it with its runtime root.
/// 构建一个加载了运行 `source` 的技能 `skill_id` 的单 VM 引擎；返回引擎及其运行根目录。
fn fixture_engine(label: &str, skill_id: &str, source: &str) -> (LuaEngine, PathBuf) {
    let runtime_root = make_temp_runtime_root(label);
    if runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime_root);
    }
    let root = RuntimeSkillRoot {
        name: "ROOT".to_string(),
        skills_dir: runtime_root.join("skills"),
    };
    write_model_test_skill_to_root(&root.skills_dir, skill_id, source);
    let mut engine = make_runtime_test_engine();
    engine
        .load_from_roots(&[root])
        .expect("fixture skill should load");
    (engine, runtime_root)
}

/// Build one single-VM engine with the recursive fixture loaded; return it with its runtime root.
/// 构建一个加载了递归夹具的单 VM 引擎；返回引擎及其运行根目录。
fn recursive_engine(label: &str) -> (LuaEngine, PathBuf) {
    fixture_engine(label, "vulcan-recursive-skill", RECURSIVE_SOURCE)
}

/// Unbounded self-recursion through `vulcan.call` fails with a Lua error instead of overflowing the
/// thread stack, `pcall` can observe it, and the pooled VM's counter returns to zero afterwards.
/// 通过 `vulcan.call` 的无界自递归以 Lua 错误失败而不是耗尽线程栈，`pcall` 能观察到该错误，
/// 且之后池化 VM 的计数器回到零。
#[test]
fn nested_vulcan_call_depth_is_bounded_and_recovers() {
    let (engine, runtime_root) = recursive_engine("nested-call-depth-limit");

    let error = engine
        .call_skill(RECURSIVE_TOOL, &json!({}), None)
        .expect_err("unbounded recursion must fail");
    assert!(
        error.contains("nested call depth exceeds 32 levels"),
        "unexpected error: {error}"
    );

    let caught = engine
        .call_skill(RECURSIVE_TOOL, &json!({"catch": true}), None)
        .expect("pcall should observe the depth error");
    assert!(
        caught.content.starts_with("false|") && caught.content.contains("nested call depth"),
        "unexpected pcall result: {}",
        caught.content
    );

    // The same single pooled VM must again allow the full depth after both failures.
    // 两次失败之后，同一个单池化 VM 必须重新允许完整深度。
    let deepest = engine
        .call_skill(RECURSIVE_TOOL, &json!({"limit": 32}), None)
        .expect("32 nested calls stay within the limit");
    assert_eq!(deepest.content, "32");
    let over = engine
        .call_skill(RECURSIVE_TOOL, &json!({"limit": 33}), None)
        .expect_err("33 nested calls exceed the limit");
    assert!(over.contains("nested call depth exceeds 32 levels"));

    let _ = fs::remove_dir_all(&runtime_root);
}

/// Build a Lua table nested `depth` levels deep, holding a number at the innermost level.
/// 构建嵌套 `depth` 层的 Lua 表，最内层保存一个数字。
fn nested_table(lua: &Lua, depth: usize) -> LuaValue {
    lua.load(format!(
        "local t = 1 for i = 1, {depth} do t = {{ t }} end return t"
    ))
    .eval::<LuaValue>()
    .expect("build nested table")
}

/// Self-referencing and mutually referencing tables are rejected instead of recursing forever.
/// 自引用与相互引用的表会被拒绝，而不会无限递归。
#[test]
fn lua_value_to_json_rejects_cyclic_tables() {
    let lua = Lua::new();
    for source in [
        "local t = {} t.self = t return t",
        "local a, b = {}, {} a.b = b b.a = a return a",
        "local t = {} t[1] = t return t",
    ] {
        let value = lua.load(source).eval::<LuaValue>().unwrap();
        let error = lua_value_to_json(&value).expect_err("cycle must be rejected");
        assert!(error.contains("cyclic"), "{source}: {error}");
        let error = check_lua_json_depth(&value, MAX_JSON_NESTING_DEPTH)
            .expect_err("cycle must exceed the depth walk");
        assert!(error.contains("nesting exceeds"), "{source}: {error}");
    }
}

/// Exactly `MAX_JSON_NESTING_DEPTH` levels convert, one more is rejected, and a far deeper table
/// is rejected without overflowing the stack.
/// 恰好 `MAX_JSON_NESTING_DEPTH` 层可以转换，多一层被拒绝，远更深的表也会被拒绝且不会耗尽栈。
#[test]
fn lua_value_to_json_bounds_nesting_depth() {
    let lua = Lua::new();
    let at_limit = nested_table(&lua, MAX_JSON_NESTING_DEPTH);
    lua_value_to_json(&at_limit).expect("limit depth converts");
    check_lua_json_depth(&at_limit, MAX_JSON_NESTING_DEPTH).expect("limit depth passes");

    for depth in [MAX_JSON_NESTING_DEPTH + 1, 200_000] {
        let deep = nested_table(&lua, depth);
        let error = lua_value_to_json(&deep).expect_err("too deep must be rejected");
        assert!(error.contains("nesting exceeds 100 levels"), "{error}");
        check_lua_json_depth(&deep, MAX_JSON_NESTING_DEPTH).expect_err("too deep must fail");
    }
}

/// A table shared by several parents is not a cycle and still converts.
/// 被多个父表共享的表不是环，仍然可以转换。
#[test]
fn lua_value_to_json_accepts_shared_acyclic_references() {
    let lua = Lua::new();
    let value = lua
        .load("local leaf = { 1, 2 } return { a = leaf, b = { leaf, leaf } }")
        .eval::<LuaValue>()
        .unwrap();
    let json = lua_value_to_json(&value).expect("shared reference converts");
    assert_eq!(json, json!({"a": [1, 2], "b": [[1, 2], [1, 2]]}));
    check_lua_json_depth(&value, MAX_JSON_NESTING_DEPTH).expect("shared reference passes");
}

/// JSON text nesting `depth` arrays around the number 1.
/// 在数字 1 外嵌套 `depth` 层数组的 JSON 文本。
fn nested_json_text(depth: usize) -> String {
    format!("{}1{}", "[".repeat(depth), "]".repeat(depth))
}

/// JSON values reaching Lua obey the same 100-level limit as values leaving it, so anything a skill
/// can decode it can also encode, and over-deep tool arguments are rejected on entry.
/// 进入 Lua 的 JSON 值与离开 Lua 的值遵守同一个 100 层上限，因此技能能解码的值一定能重新编码，
/// 过深的工具参数在入口处就被拒绝。
#[test]
fn json_values_entering_lua_share_the_encode_depth_limit() {
    let source = r#"return function(args)
  local ok, value = pcall(vulcan.json.decode, args.text)
  if not ok then
    return "decode-error|" .. tostring(value)
  end
  return vulcan.json.encode(value)
end
"#;
    let (engine, runtime_root) =
        fixture_engine("json-depth-round-trip", "vulcan-json-depth-skill", source);
    let tool = "vulcan-json-depth-skill-ping";

    let at_limit = nested_json_text(MAX_JSON_NESTING_DEPTH);
    let echoed = engine
        .call_skill(tool, &json!({"text": at_limit}), None)
        .expect("limit depth round-trips");
    assert_eq!(echoed.content, at_limit);

    // 101 to 127 levels parse in serde_json but must not reach Lua, where they could not be encoded.
    // 101 到 127 层可以被 serde_json 解析，但不能进入 Lua，否则无法再编码回去。
    for depth in [MAX_JSON_NESTING_DEPTH + 1, 127] {
        let rejected = engine
            .call_skill(tool, &json!({"text": nested_json_text(depth)}), None)
            .expect("decode error is catchable");
        assert!(
            rejected.content.starts_with("decode-error|")
                && rejected.content.contains("nesting exceeds 100 levels"),
            "depth {depth}: {}",
            rejected.content
        );
    }

    let mut deep_argument = json!(1);
    for _ in 0..MAX_JSON_NESTING_DEPTH {
        deep_argument = json!([deep_argument]);
    }
    let error = engine
        .call_skill(tool, &json!({"text": "1", "deep": deep_argument}), None)
        .expect_err("over-deep arguments are rejected");
    assert!(error.contains("nesting exceeds 100 levels"), "{error}");

    let _ = fs::remove_dir_all(&runtime_root);
}

/// A table used as a key is not counted as nesting; the deserializer that follows the depth check
/// rejects it as an invalid key without descending into it, however deep the key table is.
/// 作为键的表不计入嵌套层数；深度检查之后的反序列化器会把它作为无效键拒绝，不论键表多深都不会深入其中。
#[test]
fn lua_table_keys_are_rejected_as_keys_not_as_nesting() {
    let lua = Lua::new();
    let value = lua
        .load("local k = 1 for i = 1, 200000 do k = { k } end return { [k] = 1 }")
        .eval::<LuaValue>()
        .unwrap();
    check_lua_json_depth(&value, MAX_JSON_NESTING_DEPTH).expect("keys do not nest");
    let error = lua
        .from_value::<Value>(value)
        .expect_err("table keys are not JSON keys");
    assert!(!error.to_string().contains("nesting"), "{error}");
}

/// Lua entry that, depending on `args.mode`, yields from inside a nested call, abandons a coroutine
/// mid-call, or nests `args.limit` calls deep.
/// 根据 `args.mode` 在嵌套调用内部 yield、在调用中途放弃协程，或嵌套 `args.limit` 层调用的 Lua 入口。
const COROUTINE_SOURCE: &str = r#"local TOOL = "vulcan-coroutine-skill-ping"
return function(args)
  if args.mode == "yield" then
    coroutine.yield("escaped")
    return "resumed"
  end
  if args.mode == "nest" then
    local depth = args.depth or 0
    if depth >= args.limit then
      return tostring(depth)
    end
    return vulcan.call(TOOL, { mode = "nest", depth = depth + 1, limit = args.limit })
  end
  local co = coroutine.create(function()
    return vulcan.call(TOOL, { mode = "yield" })
  end)
  local ok, value = coroutine.resume(co)
  co = nil
  collectgarbage()
  return tostring(ok) .. "|" .. tostring(value)
end
"#;

/// A coroutine that tries to yield out of a nested `vulcan.call` and is then abandoned never leaves
/// the nested-call counter raised: the pooled VM still allows the full 32 levels afterwards.
/// 尝试从嵌套 `vulcan.call` 中 yield 并随后被放弃的协程不会让嵌套调用计数器保持升高：
/// 之后池化 VM 仍然允许完整的 32 层。
#[test]
fn nested_vulcan_call_depth_survives_abandoned_coroutines() {
    let (engine, runtime_root) = fixture_engine(
        "nested-call-depth-coroutine",
        "vulcan-coroutine-skill",
        COROUTINE_SOURCE,
    );
    let tool = "vulcan-coroutine-skill-ping";
    for _ in 0..3 {
        let outcome = engine
            .call_skill(tool, &json!({}), None)
            .expect("driver returns");
        // Yielding across the Rust dispatcher fails inside the coroutine instead of suspending it.
        // 跨 Rust 分发器 yield 会在协程内部失败，而不会把协程挂起。
        assert!(
            outcome.content.starts_with("false|"),
            "unexpected coroutine outcome: {}",
            outcome.content
        );
    }
    let deepest = engine
        .call_skill(tool, &json!({"mode": "nest", "limit": 32}), None)
        .expect("full depth is still available");
    assert_eq!(deepest.content, "32");
    let _ = fs::remove_dir_all(&runtime_root);
}
