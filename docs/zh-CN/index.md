# LuaSkills 中文文档

[English](../index.md) | [简体中文](index.md) | [日本語](../ja/index.md) | [한국어](../ko/index.md) | [Español](../es/index.md) | [Français](../fr/index.md) | [Deutsch](../de/index.md) | [Português (BR)](../pt-BR/index.md)

[English README](../../README.md) | [中文首页](../../README.zh-CN.md)

这里是 LuaSkills 的中文技术文档入口。
根目录 README 已调整为英文产品首页，中文读者可以从本页进入完整技术文档。

## 推荐阅读路径

| 读者类型 | 推荐入口 |
| --- | --- |
| 第一次了解项目 | [中文首页](../../README.zh-CN.md) |
| 想了解产品定位与能力边界 | [为什么是 LuaSkills](product/why-luaskills.md) |
| Skill 作者 | [Lua Skill 开发手册](skill-development.md) |
| 从 0.4.4 升级到 0.4.6 | [LuaSkills 0.4.6 升级说明](../upgrade-0.4.6.md) |
| 从 0.4.6 升级到 0.5.0 | [LuaSkills 0.5.0 破坏性升级说明](../upgrade-0.5.0.md) |
| 从 0.5.0 升级到 0.5.1 | [LuaSkills 0.5.1 受管运行时根升级说明](../upgrade-0.5.1.md) |
| 从 0.5.1 升级到 0.5.2 | [LuaSkills 0.5.2 补丁升级说明](../upgrade-0.5.2.md) |
| 从 0.5.2 升级到 0.5.3 | [LuaSkills 0.5.3 补丁升级说明](../upgrade-0.5.3.md) |
| 从 0.5.3 升级到 0.5.4 | [LuaSkills 0.5.4 补丁升级说明](../upgrade-0.5.4.md) |
| 采用 0.5.5 技能包配置正式契约 | [LuaSkills 0.5.5 技能包配置升级说明](../upgrade-0.5.5.md) |
| 第一次做 FFI 联调 | [FFI 宿主接入检查清单](ffi/host-checklist.md) |
| 需要完整 FFI 参数、内存和生命周期规则 | [FFI 对接文档](ffi/integration-guide.md) |
| 需要接入 System Plugin 受管 Python/Node 长期会话 | [System Plugin 受管运行时使用指南](system-plugin-managed-runtime.md) |
| 宿主需要共享 Python/Node 发行包或配置 B3-B7 | [宿主指定受管运行时根目录](managed-runtime-host-roots.md) |
| 从 0.5.0 升级宿主、FFI 或 SDK | [LuaSkills 0.5.1 升级说明](../upgrade-0.5.1.md) |
| 需要参考历史 beta 发布边界 | [FFI Beta 发布说明](ffi/beta-release-notes.md) |
| 需要做 `runtime_lease`、`system_runtime_lease` 或 `host_result` 联调 | [FFI 对接文档](ffi/integration-guide.md) |
| 需要接管 SQLite / LanceDB | [宿主数据库 Provider 对接说明](providers/host-database-provider-guide.md) |
| 需要理解 ROOT / PROJECT / USER | [Skill Root 层级与管理边界](architecture/skill-root-layer-policy.md) |
| 需要设计 skill 安装来源与 Hub | [Skill 来源策略、官方 Hub 与进度事件](architecture/skill-source-policy-and-hub.md) |
| 需要声明、读取或对接技能包配置 | [Skill 包级配置声明、运行时控制与宿主对接](architecture/skill-config-system-design.md) |
| 需要理解宿主结构化结果、`system_lua_lib` 与执行平面 | [宿主工具结果桥接、宿主 LuaRuntime（`system_lua_lib`）与执行平面设计稿](architecture/host-tooling-result-bridge-design.md) |

## 产品与生态

- [为什么是 LuaSkills](product/why-luaskills.md)
- [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit)：重要的真实 LuaSkills 示例。
- [demo-skill](https://github.com/LuaSkills/demo-skill)：标准 skill 仓库模板。
- [luaskills-sdk-typescript](https://github.com/LuaSkills/luaskills-sdk-typescript)：TypeScript / Node.js SDK。
- [luaskills-sdk-python](https://github.com/LuaSkills/luaskills-sdk-python)：Python SDK。
- [luaskills-sdk-go](https://github.com/LuaSkills/luaskills-sdk-go)：Go SDK。

## Skill 开发

- [Lua Skill 开发手册](skill-development.md)
- [System Plugin 受管运行时使用指南](system-plugin-managed-runtime.md)
- [宿主指定受管运行时根目录](managed-runtime-host-roots.md)
- [LuaSkills 0.5.5 技能包配置升级说明](../upgrade-0.5.5.md)
- [LuaSkills 0.5.4 补丁升级说明](../upgrade-0.5.4.md)
- [LuaSkills 0.5.3 补丁升级说明](../upgrade-0.5.3.md)
- [LuaSkills 0.5.2 补丁升级说明](../upgrade-0.5.2.md)
- [LuaSkills 0.5.1 升级说明](../upgrade-0.5.1.md)
- [Skill Root 层级与管理边界](architecture/skill-root-layer-policy.md)
- [Skill 包级配置声明、运行时控制与宿主对接](architecture/skill-config-system-design.md)
- [宿主工具结果桥接、宿主 LuaRuntime（`system_lua_lib`）与执行平面设计稿](architecture/host-tooling-result-bridge-design.md)

Skill 作者最应该记住的边界是：skill 应依赖 `vulcan.context.*` 和 `vulcan.deps.*` 暴露的协议路径，不应该反推宿主物理目录结构。

### Skill 命名规则

`skill_id` 和每个 `entry.name` 都必须匹配 `^[a-z]([a-z0-9-]*[a-z0-9])?$`。物理 skill 目录名是唯一 `skill_id` 来源，`skill.yaml` 不能声明 `skill_id` 字段。canonical entry 名称为 `{skill_id}-{entry_name}`，冲突时可能追加稳定的 `-N` 后缀。GitHub 托管 skill 的仓库派生或显式 `skill_id`、release zip 前缀、checksum 前缀、zip 顶层目录和最终安装目录必须完全一致；发布资产使用 `{skill_id}-v{version}-skill.zip`、`{skill_id}-v{version}-checksums.txt`，zip 内必须包含 `{skill_id}/skill.yaml`。

## 宿主与 FFI 接入

- [FFI Beta 发布说明](ffi/beta-release-notes.md)
- [LuaSkills 0.4.6 升级说明](../upgrade-0.4.6.md)
- [LuaSkills 0.5.0 破坏性升级说明](../upgrade-0.5.0.md)
- [LuaSkills 0.5.5 技能包配置升级说明](../upgrade-0.5.5.md)
- [LuaSkills 0.5.4 补丁升级说明](../upgrade-0.5.4.md)
- [LuaSkills 0.5.3 补丁升级说明](../upgrade-0.5.3.md)
- [LuaSkills 0.5.2 补丁升级说明](../upgrade-0.5.2.md)
- [LuaSkills 0.5.1 受管运行时根升级说明](../upgrade-0.5.1.md)
- [FFI 宿主接入检查清单](ffi/host-checklist.md)
- [FFI 对接文档](ffi/integration-guide.md)
- [System Plugin 受管运行时使用指南](system-plugin-managed-runtime.md)
- [宿主指定受管运行时根目录](managed-runtime-host-roots.md)
- [宿主数据库 Provider 对接说明](providers/host-database-provider-guide.md)

第一次接入时建议先跑通 `version -> engine_new -> load_from_roots -> list_entries -> call_skill -> run_lua -> engine_free`，再继续接 lifecycle、query helper、install/update/uninstall、provider callback 或 `space_controller`。

## 架构与设计

- [Skill Root 层级与管理边界](architecture/skill-root-layer-policy.md)
- [Skill 包级配置声明、运行时控制与宿主对接](architecture/skill-config-system-design.md)
- [宿主工具结果桥接、宿主 LuaRuntime（`system_lua_lib`）与执行平面设计稿](architecture/host-tooling-result-bridge-design.md)

## 历史与归档

- [FFI 收敛改造草案](archive/ffi-refactor-draft.md)

归档文档只保留历史设计背景。实际接入请优先阅读当前 FFI、Provider 与 Skill 文档。

## 本地示例

- [C FFI Demo](../../examples/ffi/c/README.md)
- [TypeScript 标准 FFI 示例](../../examples/ffi/typescript/README.md)
- [Standard Runtime FFI Fixture](../../examples/ffi/standard_runtime/README.md)
- [FFI Demo Runtime](../../examples/ffi/demo_runtime/README.md)
- [Host Callback Demo](../../examples/ffi/host_provider_demo/README.md)
- [LuaSkills Rust Demo](../../examples/demo-rust/README.md)：Rust crate 直连宿主示例，覆盖 `call_skill` 与 `vulcan.host.*`。
- [受管 Python/Node 运行时示例](../../examples/managed_runtime/README.md)：Lua 一等调度，调用受管 Python 与 Node.js 子运行时。
- `cargo run --bin luaskills-debug -- inspect --runtime-root <目录> --skill-path <目录>`：仓库内单 skill 调试 bin，会先把 skill 同步进真实 `runtime_root` 再完成加载。
- 新宿主应传 `runtime_root` 作为 LuaSkills 数据布局入口。需要共享应用级 Python/Node 资产时，可额外传独立受管发行根与环境根；其他目录仍统一从 `runtime_root` 派生。
- [LuaSkills FFI Demo](../../examples/demo-ffi/README.md)
