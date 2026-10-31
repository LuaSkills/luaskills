# LuaSkills FFI Demo

该目录是面向动态库宿主的 FFI demo 入口。它不是 skill 本身，而是一个通过 `luaskills` 动态库加载示例 skill 的宿主示例。

默认运行根：

```text
examples/ffi/standard_runtime/runtime_root
```

运行前可先拉取运行依赖：

```powershell
.\examples\demo-ffi\run.ps1 -Fetch all
```

或在类 Unix 环境：

```bash
bash examples/demo-ffi/run.sh all
```

`lua` 和 `all` 目标仅安装 Lua runtime 包到 demo 运行根。数据库由宿主注册并管理。
