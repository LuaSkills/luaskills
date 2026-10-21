# LuaSkills 中文文档

[English](../index.md) | [简体中文](index.md) | [日本語](../ja/index.md) | [한국어](../ko/index.md) | [Español](../es/index.md) | [Français](../fr/index.md) | [Deutsch](../de/index.md) | [Português (BR)](../pt-BR/index.md)

[中文首页](../../README.zh-CN.md) | [为什么是 LuaSkills](product/why-luaskills.md)

文档按“你要做什么”分为三类：技能开发、宿主对接、SDK。宿主与 FFI 的深度参考以中文维护。

## 推荐阅读路径

| 读者 | 入口 |
| --- | --- |
| 第一次了解项目 | [为什么是 LuaSkills](product/why-luaskills.md) |
| 编写 Lua Skill | [Lua Skill 开发手册](development/skill-development.md) |
| 编写 System Plugin 或受管 Python/Node 流程 | [System Plugin 受管运行时使用指南](development/system-plugin-managed-runtime.md) |
| 第一次做 FFI 联调 | [FFI 宿主接入检查清单](integration/ffi-host-checklist.md) |
| 需要完整 FFI 参数、内存与生命周期规则 | [FFI 对接文档](integration/ffi-integration-guide.md) |
| 使用 TypeScript、Python 或 Go | [SDK 总览](sdk/index.md) |

## 技能开发

面向 Skill 与 System Plugin 作者。

- [Lua Skill 开发手册](development/skill-development.md)：包结构、`skill.yaml`、入口、输入 Schema 与全部 `vulcan.*` API。
- [System Plugin 受管运行时使用指南](development/system-plugin-managed-runtime.md)：严格 System Plugin 包、持久 Python/Node 会话、事件与清理。
- [demo-skill](https://github.com/LuaSkills/demo-skill)：最小技能仓库模板。
- [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit)：生产级技能示例。

Skill 作者最应该记住的边界是：依赖 `vulcan.context.*` 与 `vulcan.deps.*` 暴露的协议路径，不要反推宿主物理目录结构。

### Skill 命名规则

`skill_id` 和每个 `entry.name` 都必须匹配 `^[a-z]([a-z0-9-]*[a-z0-9])?$`。物理 skill 目录名是唯一 `skill_id` 来源，`skill.yaml` 不能声明 `skill_id` 字段。canonical entry 名称为 `{skill_id}-{entry_name}`，冲突时可能追加稳定的 `-N` 后缀。GitHub 托管 skill 的仓库派生或显式 `skill_id`、release zip 前缀、checksum 前缀、zip 顶层目录和最终安装目录必须完全一致；发布资产使用 `{skill_id}-v{version}-skill.zip`、`{skill_id}-v{version}-checksums.txt`，zip 内必须包含 `{skill_id}/skill.yaml`。

## 宿主对接

面向嵌入运行时的产品。

- [FFI 宿主接入检查清单](integration/ffi-host-checklist.md)：最短联调路径与自检项。
- [FFI 对接文档](integration/ffi-integration-guide.md)：标准 C ABI 与公共 `_json` FFI 的完整参数、内存、线程与生命周期规则。
- [宿主指定受管运行时根目录](integration/managed-runtime-host-roots.md)：数据根、发行根与环境根，资源策略与解析器。
- [宿主数据库 Provider 对接说明](integration/host-database-provider-guide.md)：SQLite / LanceDB 的动态库、宿主回调与 space controller 模式。
- [Skill Root 层级与管理边界](integration/skill-root-layer-policy.md)：`ROOT` / `PROJECT` / `USER`。
- [Skill 来源策略、官方 Hub 与进度事件](integration/skill-source-policy-and-hub.md)
- [Skill 包级配置声明、运行时控制与宿主对接](integration/skill-config-system-design.md)
- [宿主工具结果桥接、宿主 LuaRuntime（`system_lua_lib`）与执行平面](integration/host-tooling-result-bridge-design.md)
- 英文概览：[运行时架构](../integration/runtime-model.md)、[FFI 与对接总览](../integration/ffi-overview.md)、[技能包配置宿主契约](../integration/package-configuration.md)、[数据库 Provider 概览](../integration/database-providers.md)
- 机器契约：[嵌入式运行时](../../contracts/embedded-runtime.md)、[嵌入式 FFI](../../contracts/embedded-ffi.md)、[System 租约执行](../../contracts/system-lease-execution.md)

第一次接入时建议先跑通 `version -> engine_new -> load_from_roots -> list_entries -> call_skill -> run_lua -> engine_free`，再接 lifecycle、query helper、install/update/uninstall、provider callback 或 `space_controller`。

## SDK

- [SDK 总览](sdk/index.md)：包名、覆盖范围、版本规则与发布顺序。
- [luaskills-sdk-typescript](https://github.com/LuaSkills/luaskills-sdk-typescript)
- [luaskills-sdk-python](https://github.com/LuaSkills/luaskills-sdk-python)
- [luaskills-sdk-go](https://github.com/LuaSkills/luaskills-sdk-go)

## 本地示例

- [LuaSkills Rust Demo](../../examples/demo-rust/README.md)：Rust crate 直连宿主示例，覆盖 `call_skill` 与 `vulcan.host.*`。
- [受管 Python/Node 运行时示例](../../examples/managed_runtime/README.md)
- [技能包配置示例](../../examples/skill-package-config/README.md)
- [C FFI Demo](../../examples/ffi/c/README.md)
- [TypeScript 标准 FFI 示例](../../examples/ffi/typescript/README.md)
- [Standard Runtime FFI Fixture](../../examples/ffi/standard_runtime/README.md)
- [FFI Demo Runtime](../../examples/ffi/demo_runtime/README.md)
- [Host Callback Demo](../../examples/ffi/host_provider_demo/README.md)
- [LuaSkills FFI Demo](../../examples/demo-ffi/README.md)
- `cargo run --bin luaskills-debug -- inspect --runtime-root <目录> --skill-path <目录>`：把单个 skill 同步进真实 `runtime_root` 后按宿主方式加载。

## 维护者

- [发布流程](../../scripts/release/README.md)
- [贡献与 DCO](../../CONTRIBUTING.md)
