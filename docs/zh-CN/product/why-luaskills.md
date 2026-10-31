# 为什么是 LuaSkills

[English](../../product/why-luaskills.md) | [简体中文](why-luaskills.md) | [日本語](../../ja/product/why-luaskills.md) | [한국어](../../ko/product/why-luaskills.md) | [Español](../../es/product/why-luaskills.md) | [Français](../../fr/product/why-luaskills.md) | [Deutsch](../../de/product/why-luaskills.md) | [Português (BR)](../../pt-BR/product/why-luaskills.md)

[中文文档入口](../index.md)

LuaSkills 解决的不是“怎么运行一段 Lua”这么小的问题。
它解决的是：当一个产品真的想把脚本、工具、数据库能力、AI Agent 工作流和用户安装能力组织成长期可维护的 Skill 体系时，运行时边界应该如何稳定下来。

## 产品问题

很多宿主产品最后都会需要用户可感知的自动化能力：

- AI Agent 的本地工具。
- IDE、桌面端、开发者工具中的工作流。
- 搜索、记忆、数据库型能力。
- 产品内置的一方 skill。
- 项目级或用户级后装 skill。
- 从原型脚本走向可维护扩展生态的路径。

难点不是“把 Lua 跑起来”。
真正的难点是让 Lua skill 适合产品边界：

- 哪些 skill 已安装？
- 哪些 entry 可以被调用？
- 当前 skill 属于哪个 root？
- 当前操作携带什么 authority？
- 依赖目录由谁决定？
- 数据库 ownership 归宿主还是 runtime？
- UI 怎么解释这个 skill 能做什么？

LuaSkills 的设计就是围绕这些问题展开的。

## LuaSkills 提供什么

LuaSkills 给宿主提供一层运行时契约，而不是一堆口头约定。

它标准化了：

- Skill 包加载。
- Entry 发现与调用。
- strict help 树。
- 运行时上下文注入。
- skill 依赖路径注入。
- SQLite Provider 路由。
- system、project、user skill root 分层。
- Rust、C ABI、公共 `_json` FFI 等接入面。

因此宿主可以把它映射成命令面板、MCP tools、桌面应用功能、本地 Agent 工具、服务端能力或内部平台能力。

## 能力分类

| 分类 | 解决的问题 |
| --- | --- |
| Runtime Core | 加载 skill、重载 root、列出 entry、调用 skill 函数。 |
| Skill Authoring | Skill 作者用稳定的 `vulcan.*` API 与结构化 help 编写能力。 |
| Product Control | 权限、预算、UI、authority、用户提示继续由宿主控制。 |
| Data-aware Skills | SQLite 由宿主注册回调管理。 |
| Multi-language Hosts | Rust、C ABI、TypeScript、Python、Go 或混合宿主都能接入同一套模型。 |
| Ecosystem Growth | 用真实示例和模板沉淀 skill 形态，而不是每个项目重新发明目录结构。 |

## 多语言宿主支持

LuaSkills 支持多种宿主形态：

- Rust 宿主可以直接调用 crate。
- C / C++ 等低层宿主可以使用标准 C ABI。
- Python、Node.js、TypeScript 等动态宿主可以使用公共 `_json` FFI。
- TypeScript、Python、Go 用户可以优先使用对应 SDK。

这样产品团队可以根据自己的技术栈选择接入层，而不用改变 skill 的基本模型。

## Skill 生态

两个仓库尤其重要：

- [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit)：真实、重要的 LuaSkills 示例，展示源码导航、AST 检查、结构化搜索、Markdown 导航和 patch 工作流如何作为 skill 产品交付。
- [demo-skill](https://github.com/LuaSkills/demo-skill)：标准 skill 仓库模板，用来学习最小目录结构、`skill.yaml`、runtime entry 和 help 文件。

推荐学习路径：

1. 用 `demo-skill` 学会标准 skill 包形态。
2. 用 `vulcan-codekit` 理解真实产品级 skill 怎么组织。
3. 用 LuaSkills 把自己的宿主接入同一套运行时契约。

## 信任与控制

LuaSkills 不把任意 Lua 包描述成天然安全。
当前运行时默认把 skill 当作受信代码执行。

这是一条清晰的产品边界：LuaSkills 专注运行时正确性，安全策略由宿主决定。

宿主应该负责：

- 哪些 root 可写。
- 哪些 skill 可以安装。
- 哪些操作需要 system authority。
- 哪些数据库 provider 模式被允许。
- 哪些用户能看到哪些工具。

## 什么时候适合用 LuaSkills

适合使用 LuaSkills 的场景：

- 需要稳定、可复用的 skill 包结构。
- 需要运行时管理 help 与 entry 元数据。
- 需要宿主控制权限与产品展示。
- 需要本地或嵌入式工具执行能力。
- 需要数据库感知型 skill。
- 需要从内部工具走向公开 skill 生态。

如果你只需要一次性脚本执行器，或者今天就需要面向任意不可信代码的强沙箱，LuaSkills 不是那个层面的解决方案。

## 一句话结论

LuaSkills 是把 Lua 包变成产品级 skill 的运行时层。
它让 skill 可迁移，让宿主保持控制，让多语言集成有一条共同路径。
