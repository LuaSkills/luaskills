# 调试工具的 SQLite 宿主

`luaskills-debug` 自己注册本地 SQLite 宿主。技能清单声明 `sqlite.enable: true` 后，可以直接使用 SQL 脚本、带参数批量事务和查询，不需要 vldb 控制器、Python 或额外数据库动态库。SQLite 引擎静态包含在调试可执行文件中；LuaSkills 默认核心库仍仅接受宿主注册。

## 构建和运行

```powershell
rtk cargo build --locked --release --features debug-sqlite --bin luaskills-debug
rtk proxy .\target\release\luaskills-debug.exe call --runtime-root D:\debug-runtime --skill-path D:\skills\my-skill --tool run --output content
```

从源码运行使用 `cargo run --features debug-sqlite --bin luaskills-debug -- ...`。下载的调试可执行文件无需设置此构建选项。构建默认核心库使用 `cargo build --locked --release --lib`；发布流水线分别构建调试器和默认核心库，不用调试功能生成核心 SDK。

Windows 发布构建仅在编译调试器时设置 `RUSTFLAGS=-C target-feature=+crt-static`，随后恢复原值再构建核心库，因此调试包不要求额外安装 VC++ 运行库。MSVC 调试器内嵌 UTF-8 进程清单，保证中文解包目录产生的 LuaJIT 默认搜索路径也是 UTF-8；该设置适用于 Windows 10 1903 及以上，不修改嵌入式宿主进程的设置。

## 调试发布包验收

将技能目录放入包内 `skills/` 后，使用 `debug.ps1 -Command call -Tool <工具名>` 或 `bash debug.sh call --tool <工具名>` 即可调用 SQLite，无需先执行 `setup_runtime`。只有技能额外声明的 Lua 依赖才需要初始化。Windows 批处理入口传复杂 JSON 时推荐 `-ArgsFile`；PowerShell 的 `-ArgsJson` 和 Bash 的 `--args-json` 会通过临时 UTF-8 文件完整转发，并在退出后清理。

发布流水线在计算候选包摘要及上传之前，使用 `scripts/release/verify_debug_package.py` 解压真实归档，在含中文和空格的目录中运行包内启动器。验收覆盖技能自动发现、显式路径及已同步标识，执行脚本、参数化事务、失败回滚和跨进程重开读取；不运行依赖初始化，并阻止常规 HTTP 下载。校验器使用 Python 仅作为构建验收工具，调试包运行 SQLite 不依赖 Python。

```powershell
rtk proxy python -X utf8 scripts/release/verify_debug_package.py --archive target/release-packages/luaskills-debug-tool-windows-x64.tar.gz --platform windows-x64 --bash "C:/Program Files/Git/bin/bash.exe"
```

Linux/macOS 使用对应平台参数和原生归档，省略 `--bash`。Windows 本机验收不替代 Linux/macOS 原生验收。

```yaml
sqlite:
  enable: true
```

数据库位于 `<runtime-root>/databases/sqlite/<skill-id>/<skill-id>.sqlite3`。位置来自运行时生成的技能绑定，输入协议不接受另一个数据库路径。连接由调试器引擎持有，打开已有文件不会重置数据；引擎关闭时释放连接，未提交的显式事务由 SQLite 回滚。外键检查开启，数据库繁忙等待最多五秒。

## SQL 协议

`execute_script` 输入为 `{sql, params?}`。没有参数或参数数组为空时，SQL 可以包含多条语句；逐条执行，事务由 SQL 显式控制。带非空参数时只能包含一条语句。缺失参数、数量不符或未知输入字段均报错。返回 `{changes, last_insert_rowid}`；脚本的 `changes` 包含触发器产生的变更。

```lua
vulcan.sqlite.execute_script({
    sql = "CREATE TABLE IF NOT EXISTS notes(id INTEGER PRIMARY KEY, note TEXT NOT NULL)"
})
vulcan.sqlite.execute_script({
    sql = "INSERT INTO notes(note) VALUES (?)",
    params = {"中文内容"}
})
```

`execute_batch` 输入为 `{statements = {{sql, params?}, ...}}`。每个条目必须是一条 SQL 语句，整个批次在单个事务内提交；任何错误都会回滚全部批次写入。批次中禁止 `BEGIN`、`COMMIT`、`ROLLBACK` 和保存点控制语句，防止绕过原子性；需要手动控制事务时使用 `execute_script`。返回 `{changes, last_insert_rowid}`，批次的 `changes` 为各条语句直接影响的行数之和。

```lua
vulcan.sqlite.execute_batch({statements = {
    {sql = "INSERT INTO notes(note) VALUES (?)", params = {"第一条"}},
    {sql = "INSERT INTO notes(note) VALUES (?)", params = {"第二条"}}
}})
```

`query_json` 输入为 `{sql, params?}`，只接受一条产生结果列的 SQL，返回 `{columns, rows, row_count}`。`rows` 的每行以列名为键，因此重复列名必须通过 `AS` 设置不同别名。支持查询及带 `RETURNING` 的语句。

```lua
-- Read the real host row set and serialize it as the skill result.
-- 读取实际宿主行集并将其序列化为技能结果。
local result = vulcan.sqlite.query_json({
    sql = "SELECT id, note FROM notes WHERE note = ? ORDER BY id",
    params = {"中文内容"}
})
return vulcan.json.encode(result)
```

参数是有序数组：空值对应 SQLite NULL，布尔值对应 0/1，数字、字符串对应 SQLite 数值及文本，嵌套的 0 至 255 字节数组对应 BLOB。JSON 对象不是 SQL 标量，不会被静默序列化。结果中的 BLOB 返回字节数组；无效 UTF-8 文本及非有限浮点数明确报错。

## 支持边界

此宿主提供 `execute_script`、`execute_batch`、`query_json`。原生 FTS5 可以直接通过 SQL 的 `CREATE VIRTUAL TABLE ... USING fts5`、`MATCH` 和 `bm25` 使用。

查询流、自定义分词词典及 `ensure_fts_index` 等宿主专属便捷动作没有在调试器中定义协议，调用时明确报错。它们仍属于其他宿主可以自行提供的能力，调试器不会把它们报告为已实现。LanceDB 支持没有恢复。
