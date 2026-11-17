# LuaSkills Rust Demo

此示例使用当前检出源码。v0.9.1 需要 Rust 1.99.1，工具链由仓库根目录选择。

该目录是非 FFI 模式 demo：宿主直接通过 Rust crate 引用 `luaskills`，不经过 C ABI 动态库。

当前 demo 覆盖两条主链路：

- `call_skill`：加载共享运行时夹具里的 `demo-standard-ffi-skill-ping` 并调用 skill entry。
- `vulcan.host.*`：Rust 宿主注册 mock `model.embed` 工具，Lua 侧通过 `list / has / has_tool / call` 访问宿主工具。

`model.embed` 是本地确定性 mock，不会请求真实模型、网络、API Key 或 stream；返回结果中的 `meta.stream=false`、`meta.thinking=false` 用于演示宿主侧固定关闭 stream 与思考输出的策略。

运行前可先拉取依赖：

```powershell
.\examples\demo-rust\run.ps1 -Fetch all
```

或在类 Unix 环境：

```bash
bash examples/demo-rust/run.sh all
```

该 demo 使用 `examples/ffi/standard_runtime/runtime_root` 作为共享运行根，只是宿主接入方式从 FFI 改为 Rust 直连。

直接运行：

```powershell
.\examples\demo-rust\run.ps1
```

输出会先打印 skill 调用结果，然后打印 host-tool bridge 的结构化 JSON，例如：

```json
{
  "called_ok": true,
  "embedding": [
    0.107,
    0.092,
    0.093,
    0.095
  ],
  "first_tool": "model.embed",
  "has_embed": true,
  "has_embed_alias": true,
  "has_missing": false,
  "input": "hello from rust host",
  "model": "mock-embedding",
  "stream": false,
  "thinking": false,
  "tool_count": 1
}
```
