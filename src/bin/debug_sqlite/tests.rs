//! Real SQLite and debugger-registration regressions, using task-owned temporary storage.
//! 使用测试拥有的临时存储验证真实 SQLite 与调试器注册回归。

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

/// Own one temporary root until every engine and connection in the test has been dropped.
/// 拥有一个临时根，直到测试内全部引擎和连接被释放。
struct Fixture {
    /// Canonical temporary parent verified before recursive cleanup.
    /// 递归清理之前核对的规范临时父目录。
    parent: PathBuf,
    /// Unique root created exclusively by this fixture.
    /// 仅由此夹具创建的唯一根目录。
    root: PathBuf,
}

impl Fixture {
    /// Create a unique private root without accepting caller paths; return its owner.
    /// 创建唯一私有根，不接受调用方路径；返回所有者。
    fn new() -> Self {
        // The monotonic sequence isolates parallel tests within this process.
        // 单调序列隔离此进程内的并行测试。
        static NEXT: AtomicU64 = AtomicU64::new(0);
        // Canonicalization fixes the exact cleanup parent before creating any data.
        // 创建任何数据前通过规范化固定精确清理父目录。
        let parent = std::env::temp_dir().canonicalize().unwrap();
        // Process identity and a counter keep fixture directories distinct.
        // 进程身份与计数器使夹具目录互不相同。
        let root = parent.join(format!(
            "luaskills-debug-sqlite-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        Self { parent, root }
    }

    /// Write one SQLite skill named id, using the actual minimal manifest shape; return its path.
    /// 使用实际最小清单结构写入名为 id 的 SQLite 技能；返回其路径。
    fn skill(&self, id: &str) -> PathBuf {
        // Each package receives its own runtime-generated binding.
        // 每个技能包获得自己的运行时生成绑定。
        let path = self.root.join("skills").join(id);
        std::fs::create_dir_all(path.join("runtime")).unwrap();
        std::fs::write(path.join("skill.yaml"), format!("name: {id}\nversion: 0.1.0\nsqlite:\n  enable: true\nentries:\n  - name: sql\n    lua_entry: runtime/sql.lua\n    lua_module: debug_sqlite\n")).unwrap();
        std::fs::write(
            path.join("runtime/sql.lua"),
            r#"
-- Dispatch the explicit test action and return the actual host response as JSON.
-- 分派显式测试动作，并将实际宿主响应作为 JSON 返回。
return function(args)
    return vulcan.json.encode(vulcan.sqlite[args.action](args.input))
end
"#,
        )
        .unwrap();
        path
    }

    /// Prepare the actual debugger path for id; return its engine and listing state.
    /// 为 id 准备真实调试器路径；返回其引擎和列表状态。
    fn prepare(&self, id: &str) -> crate::PreparedDebugRuntime {
        // CLI parsing and preparation are the production entrypoints used by the executable.
        // CLI 解析与准备是可执行程序使用的生产入口。
        let command = crate::parse_debug_cli(&[
            "call".into(),
            "--runtime-root".into(),
            self.root.to_str().unwrap().into(),
            "--skill-id".into(),
            id.into(),
            "--tool".into(),
            "sql".into(),
        ])
        .unwrap();
        crate::prepare_debug_runtime(&command).unwrap()
    }
}

impl Drop for Fixture {
    /// Remove only the verified task-owned child root; no arguments or return value.
    /// 仅删除经核对的测试拥有子目录；无参数或返回值。
    fn drop(&mut self) {
        assert_eq!(self.root.parent(), Some(self.parent.as_path()));
        assert!(
            self.root
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("luaskills-debug-sqlite-")
        );
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

/// Call action with input through a prepared debugger's real Lua skill; return the parsed host response.
/// 经准备好的调试器真实 Lua 技能，以 input 调用 action；返回解析后的宿主响应。
fn call(
    prepared: &crate::PreparedDebugRuntime,
    action: &str,
    input: Value,
) -> Result<Value, String> {
    // Resolve the stable entry name from the actual debugger skill identity.
    // 从真实调试器技能身份解析稳定入口名称。
    let result = prepared
        .engine
        .call_skill(
            &format!("{}-sql", prepared.skill_id),
            &json!({"action":action,"input":input}),
            None,
        )
        .map_err(|error| error.to_string())?;
    serde_json::from_str(&result.content).map_err(|error| error.to_string())
}

/// Verify actual debugger registration, data persistence and per-skill isolation; return no value.
/// 验证实际调试器注册、数据持久化及技能隔离；无返回值。
#[test]
fn debugger_sqlite_persists_and_isolates_skill_databases() {
    // Declare storage first so connections are dropped before cleanup.
    // 先声明存储，使连接在清理前被释放。
    let fixture = Fixture::new();
    fixture.skill("first");
    fixture.skill("second");
    {
        // The production debugger must register its provider without any global setter.
        // 生产调试器必须注册自己的提供者，不调用任何全局设置器。
        let prepared = fixture.prepare("first");
        call(&prepared, "execute_script", json!({"sql":"CREATE TABLE notes(id INTEGER PRIMARY KEY, note TEXT); INSERT INTO notes VALUES (1, 'retained');"})).unwrap();
        call(
            &prepared,
            "execute_script",
            json!({"sql":"INSERT INTO notes VALUES (?, ?)","params":[2,"中文'\0尾"]}),
        )
        .unwrap();
    }
    // Reopen through a fresh engine, proving committed data survives host destruction.
    // 经全新引擎重新打开，证明已提交数据在宿主析构后保留。
    let reopened = fixture.prepare("first");
    assert_eq!(
        call(
            &reopened,
            "query_json",
            json!({"sql":"SELECT note FROM notes ORDER BY id"})
        )
        .unwrap()["rows"],
        json!([{"note":"retained"},{"note":"中文'\0尾"}])
    );
    // Exercise the nested batch input through Lua, including a rollback after a duplicate key.
    // 经 Lua 验证嵌套批次输入，包括重复键之后的回滚。
    assert_eq!(
        call(
            &reopened,
            "execute_batch",
            json!({"statements":[{"sql":"INSERT INTO notes VALUES (?, ?)","params":[3,"batch"]}]})
        )
        .unwrap()["changes"],
        1
    );
    assert!(call(&reopened, "execute_batch", json!({"statements":[{"sql":"INSERT INTO notes VALUES (4, 'rollback')"},{"sql":"INSERT INTO notes VALUES (1, 'duplicate')"}]})).is_err());
    assert_eq!(
        call(
            &reopened,
            "query_json",
            json!({"sql":"SELECT count(*) AS total FROM notes"})
        )
        .unwrap()["rows"][0]["total"],
        3
    );
    // A different skill cannot see the first skill's table through its own binding.
    // 不同技能不能通过自己的绑定看到第一个技能的表。
    let second = fixture.prepare("second");
    assert!(
        call(&second, "query_json", json!({"sql":"SELECT * FROM notes"}))
            .unwrap_err()
            .contains("no such table")
    );
    assert!(
        fixture
            .root
            .join("databases/sqlite/first/first.sqlite3")
            .is_file()
    );
    assert!(
        fixture
            .root
            .join("databases/sqlite/second/second.sqlite3")
            .is_file()
    );
}

/// Verify parameter types and query cells without precision loss; return no value.
/// 验证参数类型与查询单元格不丢失精度；无返回值。
#[test]
fn sqlite_parameters_preserve_scalars_and_blobs() {
    // This isolated connection exercises the exact production SQL converters.
    // 此隔离连接验证精确的生产 SQL 转换器。
    let connection = Connection::open_in_memory().unwrap();
    // The byte-array parameter is a blob, including its embedded zero byte.
    // 字节数组参数是二进制值，包括其中的零字节。
    let input: SqlInput = serde_json::from_value(json!({"sql":"SELECT ? AS n, ? AS b, ? AS i, ? AS f, ? AS t, ? AS bytes","params":[null,true,i64::MAX,1.25,"中文\0尾",[0,255,1]]})).unwrap();
    assert_eq!(
        query_json(&connection, &input).unwrap()["rows"][0],
        json!({"n":null,"b":1,"i":i64::MAX,"f":1.25,"t":"中文\0尾","bytes":[0,255,1]})
    );
    assert!(sql_value(&json!(u64::MAX)).is_err());
    assert!(sql_value(&json!({"unrecognized":1})).is_err());
    assert!(sql_value(&json!([256])).is_err());
    // Missing script parameters must fail instead of writing implicit NULL values.
    // 缺少脚本参数必须失败，不能写入隐式 NULL 值。
    connection
        .execute_batch("CREATE TABLE missing_bindings(value)")
        .unwrap();
    let missing: SqlInput =
        serde_json::from_value(json!({"sql":"INSERT INTO missing_bindings VALUES (?)"})).unwrap();
    assert!(execute_script(&connection, &missing).is_err());
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM missing_bindings", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

/// Verify SQL failures and transaction-control attempts roll back all batch writes; return no value.
/// 验证 SQL 失败和事务控制尝试会回滚全部批量写入；无返回值。
#[test]
fn sqlite_batches_are_atomic_and_cannot_commit_early() {
    // A single in-memory connection exposes rollback state without filesystem effects.
    // 单个内存连接暴露回滚状态，不产生文件系统效果。
    let mut connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch("CREATE TABLE items(id INTEGER PRIMARY KEY)")
        .unwrap();
    for failing_sql in [
        "INSERT INTO missing VALUES (2)",
        "COMMIT",
        "ROLLBACK",
        "SAVEPOINT escape",
    ] {
        // The first write must never survive an error in the second batch statement.
        // 第二条批量语句发生错误时，第一条写入绝不能保留。
        let input: BatchInput = serde_json::from_value(json!({"statements":[{"sql":"INSERT INTO items VALUES (?)","params":[1]},{"sql":failing_sql}]})).unwrap();
        assert!(execute_batch(&mut connection, &input).is_err());
        assert!(connection.is_autocommit());
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM items", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    // Successful writes remain parameterized and commit together after the failed attempts.
    // 失败尝试之后，成功写入仍使用参数并一起提交。
    let input: BatchInput = serde_json::from_value(json!({"statements":[{"sql":"INSERT INTO items VALUES (?)","params":[2]},{"sql":"INSERT INTO items VALUES (?)","params":[3]}]})).unwrap();
    assert_eq!(
        execute_batch(&mut connection, &input).unwrap()["changes"],
        2
    );
}

/// Verify invalid requests and unsupported host-specific actions fail explicitly; return no value.
/// 验证无效请求和不支持的宿主专属动作明确失败；无返回值。
#[test]
fn debugger_sqlite_rejects_invalid_input_and_unsupported_actions() {
    // No action may silently accept a caller-selected database path.
    // 任何动作都不能静默接受调用方选择的数据库路径。
    let fixture = Fixture::new();
    fixture.skill("invalid");
    // Construction alone does not create the SQLite database.
    // 仅构造引擎不会创建 SQLite 数据库。
    let prepared = fixture.prepare("invalid");
    assert!(
        call(
            &prepared,
            "query_json",
            json!({"sql":"SELECT 1", "database_path":"other.db"})
        )
        .unwrap_err()
        .contains("unknown field")
    );
    assert!(
        call(&prepared, "query_stream", json!({"sql":"SELECT 1"}))
            .unwrap_err()
            .contains("does not support")
    );
    assert!(!fixture.root.join("databases/sqlite").exists());
    for sql in [
        "SELECT 1 AS duplicate, 2 AS duplicate",
        "SELECT 1; SELECT 2",
        "SELECT ?",
        "SELECT 1e999 AS n",
        "SELECT CAST(x'ff' AS TEXT) AS t",
    ] {
        assert!(
            call(&prepared, "query_json", json!({"sql":sql})).is_err(),
            "{sql}"
        );
    }
}

/// Verify native FTS5 SQL remains available without the removed controller APIs; return no value.
/// 验证原生 FTS5 SQL 无需已移除的控制器 API 仍可使用；无返回值。
#[test]
fn sqlite_native_fts5_sql_is_available() {
    // Bundled SQLite supplies its normal FTS5 virtual table implementation.
    // 捆绑 SQLite 提供其常规 FTS5 虚拟表实现。
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE VIRTUAL TABLE docs USING fts5(body); INSERT INTO docs VALUES ('hello sqlite');",
        )
        .unwrap();
    // The same query path can use MATCH and SQLite's native ranking functions.
    // 相同查询路径可以使用 MATCH 和 SQLite 原生排序函数。
    let input: SqlInput = serde_json::from_value(json!({"sql":"SELECT body, bm25(docs) AS rank FROM docs WHERE docs MATCH ?","params":["sqlite"]})).unwrap();
    assert_eq!(query_json(&connection, &input).unwrap()["row_count"], 1);
}
