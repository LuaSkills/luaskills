# LuaSkills

[English](README.md) | [简体中文](README.zh-CN.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (BR)](README.pt-BR.md)

[英文文档入口](docs/index.md) | [中文文档入口](docs/zh-CN/index.md) | [Skill 模板](https://github.com/LuaSkills/demo-skill) | [CodeKit 示例](https://github.com/LuaSkills/vulcan-codekit)

LuaSkills 是一个基于 Rust 的 Lua Skill 运行时，用于加载、运行和管理 Lua 编写的技能包。
它把 Skill 加载、入口调用、结构化 help、运行时能力注入、依赖路径、SQLite / LanceDB 等能力收敛成一层稳定运行时，让宿主产品可以专注于权限、展示、用户体验和业务策略。

一句话说：LuaSkills 负责运行 skill，宿主负责决定如何把这些能力变成产品功能。

## 这个项目是什么

LuaSkills 是 LuaSkills 生态的核心运行时库，适合需要“可安装、可调用、可管理”的技能系统的宿主产品。

它提供：

- Skill 发现、加载、入口枚举与调用。
- 通过 `runtime_lease` 与带 authority 的 `system_runtime_lease` 提供持久运行时租约。
- strict help 树解析，方便宿主渲染为文档、工具、命令面板或 UI。
- `vulcan.*` 与 `vulcan.runtime.*` 标准能力注入。
- 当前请求、skill 目录、资源目录、依赖路径、客户端信息等运行时上下文注入。
- 通过 `host_result` 提供宿主结构化结果桥接，第一版标准结果种类为 `change_set`。
- SQLite / LanceDB 可选绑定，支持状态型、记忆型、搜索型 skill。
- 受管 Python 与 Node.js 子运行时，Lua skill 可通过 `vulcan.runtime.python.*` 与 `vulcan.runtime.node.*` 调用。
- 宿主可独立指定只读 Python/Node 发行根与可写环境根，并通过 Rust/JSON 只读解析接口和标准 C ABI V3 复用同一资产。
- Rust API、标准 C ABI、公共 `_json` FFI 等多种接入面。
- TypeScript、Python、Go SDK 生态接入路径。

## 这个项目不是什么

LuaSkills 不试图接管宿主产品本身。

它不是：

- MCP server 本体。
- 宿主配置文件读取器。
- 客户端预算计算器。
- 产品 UI 渲染器。
- 面向任意不可信 Lua 代码的沙箱安全边界。

权限、认证、预算、用户提示、数据库落点、工具展示和产品交互都应该由宿主控制。

## 为什么需要它

如果你希望 skill 像产品能力，而不是散落脚本，LuaSkills 就很适合。

典型场景包括：

- AI Agent 需要可复用的本地工具。
- IDE、开发者工具或桌面应用需要脚本化工作流。
- 产品需要同时支持系统内置 skill、项目级 skill 和用户级 skill。
- 宿主希望用同一套运行时服务 Rust、C ABI、TypeScript、Python、Go 等多语言接入。
- 记忆、数据库、搜索、自动化类 skill 需要明确的宿主 ownership 模型。

## 核心能力

| 领域 | LuaSkills 提供的能力 |
| --- | --- |
| Skill 运行时 | 加载 skill、列出 entry、调用 entry、重载 root、执行生命周期操作。 |
| Lua API | 注入 `vulcan.call`、`vulcan.fs`、`vulcan.path`、`vulcan.process`、`vulcan.os`、`vulcan.json`、`vulcan.cache`、`vulcan.context`、`vulcan.deps`、`vulcan.sqlite`、`vulcan.lancedb`、`vulcan.runtime`。 |
| Help 模型 | 解析 strict help 树，并把结构化 help 交给宿主渲染。 |
| 宿主边界 | 产品策略、UI、预算、权限和安全提示由宿主负责。 |
| 宿主运行时租约 | 提供公共 `runtime_lease` 与带 authority 的 `system_runtime_lease`，支持持久 Lua VM 状态、宿主路径上下文和 `system_lua_lib` 风格执行面。 |
| 宿主结构化结果 | 宿主可显式开启 `host_result`，让 skill 在主文本结果之外返回 `change_set` 等第四返回值结构。 |
| 数据库 Provider | 支持 SQLite / LanceDB 的 `dynamic_library`、`host_callback`、`space_controller` 模式。 |
| 受管子运行时 | 允许 Lua skill 调用带版本、带 lock 依赖、带常驻 worker 池的 Python 与 Node.js handler，并返回结构化 stdout / stderr / error。 |
| 多语言接入 | 提供 Rust API、标准 C ABI、公共 `_json` FFI 和 SDK 接入路径。 |
| Skill Root | 支持 `ROOT`、`PROJECT`、`USER` 等分层 root，并由宿主控制管理权限。 |

## 生态仓库

- [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit)：非常重要的 LuaSkills 实战示例，展示源码导航、AST 检查、结构化搜索、Markdown 导航与安全 patch 工作流。
- [vulcan-curl](https://github.com/LuaSkills/vulcan-curl)：HTTP 请求 skill，提供结构化 GET / POST 入口和 curl 风格请求执行能力。
- [vulcan-file](https://github.com/LuaSkills/vulcan-file)：文件操作 skill，覆盖忽略规则感知的文件列表、精确文本读取和预览优先的小范围编辑。
- [vulcan-lua](https://github.com/LuaSkills/vulcan-lua)：受控 Lua 执行 skill，用于有边界地运行内联 Lua 代码或 Lua 文件任务。
- [vulcan-testkit](https://github.com/LuaSkills/vulcan-testkit)：验证路由 skill，把 build、test、lint、typecheck 输出压缩成结构化诊断。
- [vulcan-workmem](https://github.com/LuaSkills/vulcan-workmem)：项目级工作记忆 skill，用于持久化任务检查点和上下文交接。
- [demo-skill](https://github.com/LuaSkills/demo-skill)：标准 skill 仓库示例，适合学习 `skill.yaml`、runtime entry、help 文件和基础目录布局。
- [luaskills-sdk-typescript](https://github.com/LuaSkills/luaskills-sdk-typescript)：TypeScript / Node.js 高层 SDK。
- [luaskills-sdk-python](https://github.com/LuaSkills/luaskills-sdk-python)：Python 高层 SDK。
- [luaskills-sdk-go](https://github.com/LuaSkills/luaskills-sdk-go)：Go 高层 SDK。
- [vulcan-mcp](https://github.com/OpenVulcan/vulcan-mcp)：MCP 宿主与协议适配层。

## 文档入口

- [中文文档总入口](docs/zh-CN/index.md)：中文技术文档导航。
- [英文文档入口](docs/index.md)：英文产品级文档导航。
- [为什么是 LuaSkills](docs/zh-CN/product/why-luaskills.md)：产品化叙事、能力分类、适用场景和生态定位。
- [Lua Skill 开发手册](docs/zh-CN/skill-development.md)：Skill 作者应优先阅读。
- [System Plugin 受管运行时使用指南](docs/zh-CN/system-plugin-managed-runtime.md)：从严格包配置、持久 Python/Node 会话到宿主事件与关闭的完整流程。
- [宿主指定受管运行时根目录](docs/zh-CN/managed-runtime-host-roots.md)：独立根、B3-B7 引擎策略、资产身份哈希、Rust/JSON 解析与 C ABI V3。
- [LuaSkills 0.5.1 升级说明](docs/upgrade-0.5.1.md)：宿主根、资源策略、FFI、SDK 与验证迁移清单。
- [FFI 对接文档](docs/zh-CN/ffi/integration-guide.md)：非 Rust 宿主集成细节。
- [FFI 宿主接入检查清单](docs/zh-CN/ffi/host-checklist.md)：第一次联调前的最短自检路径。
- [宿主数据库 Provider 对接说明](docs/zh-CN/providers/host-database-provider-guide.md)：SQLite / LanceDB ownership 与 provider 模式。
- [Skill Root 层级与管理边界](docs/zh-CN/architecture/skill-root-layer-policy.md)：`ROOT`、`PROJECT`、`USER` 三层模型。
- [Skill 配置系统设计稿](docs/zh-CN/architecture/skill-config-system-design.md)：Skill 配置能力的设计边界。
- [宿主工具结果桥接、宿主 LuaRuntime（`system_lua_lib`）与执行平面设计稿](docs/zh-CN/architecture/host-tooling-result-bridge-design.md)：`runtime_lease`、`system_lua_lib`、`host_result` 与执行平面的完整设计边界。

## 接入路径

| 宿主类型 | 推荐方式 |
| --- | --- |
| Rust | 直接依赖 Rust crate。 |
| C / C++ / 低层宿主 | 使用标准 C ABI。 |
| TypeScript / Node.js | 优先使用 `luaskills-sdk-typescript`，底层走公共 `_json` FFI。 |
| Python | 优先使用 `luaskills-sdk-python`，底层走公共 `_json` FFI。 |
| Go | 根据 callback 与部署需求选择 `luaskills-sdk-go` 或标准 C ABI。 |
| 混合宿主 | 标准 C ABI 承载稳定主链，公共 `_json` FFI 承载动态操作或 SDK 化集成。 |

## 快速开始

Rust 宿主可直接依赖 crate：

```toml
[dependencies]
luaskills = "0.5.1"
```

仓库开发常用命令：

```bash
cargo check
cargo test --lib
```

Rust 宿主直连示例位于 [examples/demo-rust](examples/demo-rust/README.md)，覆盖 `call_skill` 和 `vulcan.host.*` 宿主工具桥接。

如果要在本地调试单个 skill 包，仓库现在也提供了一个独立 Rust bin：

```bash
cargo run --bin luaskills-debug -- call \
  --runtime-root D:/runtime \
  --skill-path D:/skills/demo-skill \
  --tool ping \
  --args-json "{\"note\":\"hello\"}"
```

`luaskills-debug` 只是一份仓库侧开发调试工具，不会新增任何 SDK API 或 FFI API。它会先把目标 skill 同步到 `runtime_root/skills/<skill_id>`，再复用正式的 `load_from_roots -> call_skill` 链路，这样依赖路径、state、database 和运行时上下文都会和真实宿主执行保持一致。

第一次学习 skill 结构，建议按这个顺序看：

1. [demo-skill](https://github.com/LuaSkills/demo-skill)
2. [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit)
3. [Lua Skill 开发手册](docs/zh-CN/skill-development.md)

第一次做 FFI 宿主接入，建议按这个顺序看：

1. [FFI Beta 发布说明](docs/zh-CN/ffi/beta-release-notes.md)
2. [FFI 宿主接入检查清单](docs/zh-CN/ffi/host-checklist.md)
3. [FFI 对接文档](docs/zh-CN/ffi/integration-guide.md)

需要接入 System Plugin 受管 Python/Node 长期会话时，在完成上述基础链路后继续阅读 [System Plugin 受管运行时使用指南](docs/zh-CN/system-plugin-managed-runtime.md)。宿主需要共享应用级运行时资产时，再阅读[宿主指定受管运行时根目录](docs/zh-CN/managed-runtime-host-roots.md)。

## Skill 命名规则

`skill_id` 和每个 `entry.name` 必须匹配 `^[a-z]([a-z0-9-]*[a-z0-9])?$`。
物理 skill 目录名是唯一 `skill_id` 来源，`skill.yaml` 不能声明 `skill_id` 字段。
canonical entry 名称为 `{skill_id}-{entry_name}`，冲突时可能追加稳定的 `-N` 后缀。
GitHub 托管 skill 的仓库派生或显式 `skill_id`、release zip 前缀、checksum 前缀、zip 顶层目录和最终安装目录必须完全一致。
发布资产使用 `{skill_id}-v{version}-skill.zip`、`{skill_id}-v{version}-checksums.txt`，zip 内必须包含 `{skill_id}/skill.yaml`。

## 信任模型

当前运行时默认把 skill 当作受信代码执行，不提供任意不可信 Lua 包的沙箱安全承诺。

宿主应该负责决定：

- 启用哪些 root。
- 安装或忽略哪些 skill。
- 暴露哪些管理动作。
- 允许哪种数据库 provider 模式。
- 每次操作携带 system authority 还是 delegated tool authority。

## 生态统一发布顺序

如果要做一次 `0.5.1` 生态统一发布，推荐顺序如下：

1. 先发布 `LuaSkills/luaskills-packages`，确保新的兼容协议线下 `lua-runtime-packages-*` 与 `lua-deps-*` 已经存在。
2. 再发布 `LuaSkills/luaskills`，完成 crate 版本以及主仓库 `luaskills-ffi-sdk-*` 和 demo 资产的 `v0.5.1` release。
3. 再发布 TypeScript SDK `@luaskills/sdk@0.5.1`。
4. 再发布 Python SDK `luaskills-sdk==0.5.1`。
5. 再发布 Go SDK module tag `v0.5.1`。
6. 最后分别运行各 SDK 仓库的 **Examples Release** 工作流，并确保对应包或 module tag 已经在上游可见。

这样可以保证安装器、示例工作流和默认 runtime 资产都只会指向已经发布完成的 packages 资产、core 资产和 SDK 包。

## 当前版本矩阵

| 组件 | 当前版本 |
| --- | --- |
| LuaSkills core、FFI SDK 与多语言 SDK 版本线 | `0.5.1` |
| Lua runtime packages 兼容协议线 | `0.1` |
| 受管 Python / uv | `3.14.6` / `0.11.28` |
| 受管 Node.js / pnpm | `24.18.0` / `11.11.0` |
| 受管运行时引擎默认值 | `Worker=4 / 空闲=60秒 / 会话=256 / 缓冲=1MiB/流 / invoke=无限制` |
| vldb-controller | `0.2.1` |
| vldb-sqlite / vldb-lancedb | `0.1.5` / `0.1.5` |

实际可执行默认值由 `scripts/deps/fetch_managed_runtimes.*`、`scripts/deps/fetch_deps.*` 与 `scripts/deps/sync_runtime_assets.*` 定义。除非宿主有意安装并声明其他受支持的精确版本，否则 `dependencies.yaml` 必须使用上表中的受管运行时和包管理器精确版本。

## License

MIT
