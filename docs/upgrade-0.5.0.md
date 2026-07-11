# LuaSkills 0.5.0 升级说明

本文面向从 `0.4.6` 升级到 `0.5.0` 的 Rust 宿主、FFI 宿主、System Plugin 与 SDK 维护者。

`0.5.0` 是破坏性版本。它统一了 Skill 与 System Plugin 的包级依赖模型，并增加跨平台受管 Python/Node 持久会话。该版本按新 API 直接迁移，不提供旧公共 Rust 符号的兼容别名或包装。

## 版本范围

- 主仓库 crate：`luaskills = 0.5.0`
- 主仓库 Rust demo：`examples/demo-rust = 0.5.0`
- 标准 C ABI 与公共 `_json` FFI：继续使用稳定函数名；新增 System Plugin 受管会话能力由现有 System lease eval 与事件接口承载

## 破坏性 Rust API 调整

### 依赖清单改为包级名称

旧公共类型 `SkillDependencyManifest` 已删除，统一使用：

```rust
use luaskills::PackageDependencyManifest;
```

该命名同时覆盖普通 Skill 与 System Plugin，避免把包级运行时声明错误绑定为 Skill 专属概念。

### 环境计划解析器不再属于公共 API

旧公开导出的 `resolve_python_env_plan` 与 `resolve_node_env_plan` 已删除。环境计划必须由 LuaSkills 在经过可信包上下文、包根原生身份、依赖清单身份与运行时安装清单校验后内部解析。宿主不得绕过包上下文自行构造解析调用。

仍公开的宿主级能力包括：

- `ManagedRuntimeEnvPlan` 及环境状态读取结构；
- `current_managed_runtime_platform_key()`；
- `current_managed_runtime_persistent_session_capability()`；
- `ManagedRuntimePersistentSessionCapability`；
- `WINDOWS_ARM_PERSISTENT_SESSION_UNSUPPORTED_REASON`。

## 持久会话平台矩阵

| 目标 | `python.session.open` / `node.session.open` |
| --- | --- |
| Windows x86_64 | 支持 |
| Linux x86_64 | 支持 |
| Linux aarch64 | 支持 |
| macOS x86_64 | 支持 |
| macOS aarch64 / Apple Silicon | 支持 |
| Windows aarch64 | 不支持；稳定原因为 `windows_arm_is_not_supported` |

Windows ARM 会在环境创建、快照创建与进程预留前失败，也不会回退使用系统 Python 或系统 Node。

## 机器可读 capability

`vulcan.runtime.python.status()` 与 `vulcan.runtime.node.status()` 的所有结果分支都包含：

```json
{
  "persistent_session": {
    "supported": true,
    "target_os": "macos",
    "target_arch": "aarch64"
  }
}
```

不支持时还包含稳定 `reason`。宿主应依据结构化字段分支，不应解析自由文本错误。

## 关闭顺序变化

`close()` 与 `kill()` 完成进程清理后会注销会话并移除该会话尚未消费的事件。若宿主必须取得最终 `exited` 或 `failed` 事件，应先通过子协议请求退出，等待并排空最终事件与输出，再调用 `close()` 完成后代进程树和快照清理。

没有协作式退出协议时，应把 `close()` 或 `kill()` 视为权威清理操作，不再要求调用后的最终事件。

## 宿主迁移检查

1. 把 Rust 代码中的 `SkillDependencyManifest` 改为 `PackageDependencyManifest`。
2. 删除宿主对 `resolve_python_env_plan` 与 `resolve_node_env_plan` 的直接调用，改由受管运行时 API 驱动。
3. 在打开持久会话前读取 `persistent_session.supported` 与 `reason`。
4. 更新关闭流程，确保需要最终事件时先观察事件再清理。
5. 在 Windows x86_64、Linux x86_64/aarch64、macOS x86_64/aarch64 原生 runner 上运行真实 Python/Node 会话验收。
6. 将 `tests/fixtures/managed_sessions` 与新增跨平台 CI 一并纳入发布源代码。

## 相关文档

- [System Plugin 受管运行时指南](zh-CN/system-plugin-managed-runtime.md)
- [FFI 对接文档](zh-CN/ffi/integration-guide.md)
- [Lua Skill 开发手册](zh-CN/skill-development.md#59-受管-python-与-node-子运行时)
