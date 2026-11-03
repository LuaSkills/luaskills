# LuaSkills SDK

[文档入口](../index.md) | [English](../../sdk/index.md)

TypeScript、Python 与 Go SDK 让非 Rust 宿主无需手写 FFI 缓冲区或 JSON 包络即可嵌入 LuaSkills。每个 SDK 位于独立仓库，各自维护 README、示例与发布流程；本页说明它们与核心的关系。

## 包

| 语言 | 包 | 仓库 | 要求 |
| --- | --- | --- | --- |
| TypeScript / Node.js | `@luaskills/sdk`（npm） | [luaskills-sdk-typescript](https://github.com/LuaSkills/luaskills-sdk-typescript) | Node.js 24 LTS 或更高 |
| Python | `luaskills-sdk`（PyPI） | [luaskills-sdk-python](https://github.com/LuaSkills/luaskills-sdk-python) | Python 3.10 或更高 |
| Go | `github.com/LuaSkills/luaskills-sdk-go` | [luaskills-sdk-go](https://github.com/LuaSkills/luaskills-sdk-go) | Go 1.22 或更高；原生调用需要 cgo |

```bash
npm install @luaskills/sdk
pip install luaskills-sdk
go get github.com/LuaSkills/luaskills-sdk-go
```

## 覆盖范围

三个 SDK 都封装公共 `_json` FFI，并提供相同的宿主模型：

- 原生库加载、拥有型缓冲与引擎生命周期。
- 正式技能根（`ROOT`、`PROJECT`、`USER`）及带授权的安装、更新、启用、禁用与卸载。
- `call_skill`、`run_lua`、公共 `runtime_lease` 与授权绑定的 `system_runtime_lease`。
- 由 `contracts/skill-config/v1/contract.json` 生成的技能包配置接口（`describe`、`get`、`set`、事件）。
- SQLite Provider 回调、`vulcan.host.*` 宿主工具回调与 `vulcan.models.*` 模型回调。
- 宿主指定受管运行时根目录与只读运行时解析器。
- 由 `contracts/embedded/v1/contract.json` 生成的嵌入式运行时传输层。
- 运行时资产安装：TypeScript 与 Python CLI 的 `install-runtime`，Go 使用随附的 `scripts/deps/sync_runtime_assets.*` 脚本。

Go SDK 提供类型化的模型请求、响应与错误结构，但进程级模型回调注册仍需要宿主自有的 cgo 桥接。

## 版本规则

- **核心补丁保持契约，契约变化开启次版本系列。** 嵌入式契约只写明发布系列（`core_series`，即 `major.minor`），不写精确核心版本，因此同一系列每个补丁的契约字节及 `contract_sha256` 都相同。核心发布工作流拒绝契约与本系列上一已发布版本不同的补丁（`scripts/release/contract_series.py`）。
- **SDK 跟随核心系列。** SDK 版本即包版本（`package.json`、`pyproject.toml`、Go 模块标签），并镜像到仓库的 `VERSION` 文件。新的核心系列随附同 `major.minor` 的 SDK；核心补丁不需要发布 SDK，仅修复 SDK 时递增该 SDK 的补丁号。
- **每个 SDK 解析本系列最新的核心补丁。** 运行时资产安装（`DEFAULT_LUASKILLS_SERIES` / `DefaultLuaSkillsSeries`）安装 SDK 所在系列最新的已发布核心版本，只有其包内契约摘要等于 SDK 的摘要时才接受，不回退到旧补丁。嵌入式兼容检查接受该系列中契约摘要、协议、ABI 与描述版本均一致的任意核心版本。宿主仍可固定精确核心标签，并按同样规则校验。
- **契约只复制、不修改。** `contracts/skill-config/v1/` 与 `contracts/embedded/v1/` 是核心仓库 `contracts/` 的精确副本，SDK 生成代码以其为准校验。
- **运行时包独立发布。** Lua 运行时包与原生依赖来自 [`LuaSkills/luaskills-packages`](https://github.com/LuaSkills/luaskills-packages) 的兼容 `0.1` 系列。

当前系列见各 SDK README 的 **Version Alignment** 一节。

## 接入方式选择

| 宿主 | 推荐路径 |
| --- | --- |
| Rust | 直接使用 `luaskills` crate，无需 SDK。 |
| TypeScript / Node.js | `@luaskills/sdk`。 |
| Python | `luaskills-sdk`。 |
| Go | `luaskills-sdk-go`；需要完全掌控回调时使用标准 C ABI。 |
| C / C++ | 标准 C ABI，见 [FFI 对接文档](../integration/ffi-integration-guide.md)。 |

## 发布顺序

SDK 消费已发布的核心产物，因此在核心之后发布：

1. 匹配的 `LuaSkills/luaskills-packages` 系列已经存在。
2. 核心 GitHub Release（五平台 FFI SDK 与示例）完成构建与验收。
3. `luaskills` crate 发布到 crates.io。
4. TypeScript 与 Python SDK 通过各自的候选与完成工作流发布，并完成冷注册表消费验证。
5. TypeScript 与 Python 链路通过 Go 前置检查后，发布 Go 模块标签。
6. 各 SDK 的包或模块标签在上游可见后，再运行 **Examples Release** 工作流。
7. [luaskills-debug](https://github.com/LuaSkills/luaskills-debug) 在自己的仓库中基于已发布的 crate 发布。

具体门禁见 [`scripts/release/README.md`](../../../scripts/release/README.md) 与 [`scripts/release/SDK_PREREQUISITES.md`](../../../scripts/release/SDK_PREREQUISITES.md)。

## 相关文档

- [FFI 对接文档](../integration/ffi-integration-guide.md)
- [FFI 宿主接入检查清单](../integration/ffi-host-checklist.md)
- [宿主指定受管运行时根目录](../integration/managed-runtime-host-roots.md)
- [Skill 包级配置声明、运行时控制与宿主对接](../integration/skill-config-system-design.md)
