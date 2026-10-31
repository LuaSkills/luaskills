# Python SQLite 宿主示例

本示例使用 Python 标准库 `sqlite3` 管理数据库，通过 LuaSkills JSON SQLite 回调处理技能请求，不需要控制器或数据库动态库。

先构建本仓库动态库，再设置 `LUASKILLS_LIB` 为该库的绝对路径并运行 `run_python_host_provider_demo.py`。Windows 示例：

```powershell
cargo build --lib
$env:LUASKILLS_LIB = (Resolve-Path target/debug/luaskills.dll).Path
python examples/ffi/host_provider_demo/run_python_host_provider_demo.py
```

示例技能写入测试备注，再读取并输出结果。数据库保存在专用演示根的 `host_managed/sqlite/ROOT-host-provider-sqlite-demo.db`，由 Python 宿主创建和关闭连接。

示例仅实现 `execute_script` 与 `query_json`，后者返回 `{ "rows": [...] }`；其他动作明确失败。正式宿主应按自己的数据库协议实现所需动作、事务与容量限制。回调返回缓冲由 LuaSkills 分配器创建，并由对应释放接口回收。

0.7.3 删除了退役数据库字段与导出符号。示例结构声明必须与同版本头文件配套，不能混用旧动态库。
