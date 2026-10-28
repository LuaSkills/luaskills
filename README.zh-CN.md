<div align="center">

# LuaSkills

**把 Lua 脚本变成产品级技能的 Rust 运行时**

Rust 核心 · LuaJIT 技能 · Rust API、C ABI、JSON FFI 与 TypeScript / Python / Go SDK

[![crates.io](https://img.shields.io/crates/v/luaskills.svg)](https://crates.io/crates/luaskills)
[![docs.rs](https://img.shields.io/docsrs/luaskills)](https://docs.rs/luaskills)
[![License: MIT](https://img.shields.io/badge/License-MIT-green.svg)](LICENSE)
[![Release](https://img.shields.io/github/v/release/LuaSkills/luaskills)](https://github.com/LuaSkills/luaskills/releases)

[English](README.md) | **简体中文** | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (BR)](README.pt-BR.md)

[中文文档](docs/zh-CN/index.md) · [Skill 模板](https://github.com/LuaSkills/demo-skill) · [CodeKit 示例](https://github.com/LuaSkills/vulcan-codekit)

</div>

> 本文是[英文 README](README.md) 的译文，两者不一致时以英文版为准。

---

## 关于项目

LuaSkills 在宿主应用内加载、运行和管理 Lua 技能包。宿主无需自建插件运行时，即可获得可脚本化的工具、结构化帮助、受管 Python 与 Node.js 运行时、依赖管理以及数据库感知的执行能力。

**LuaSkills 负责运行技能，宿主决定技能如何成为产品功能。** 策略、认证、权限确认、预算、存储位置与用户体验都留在宿主侧。

LuaSkills 是 [Vulcan Code](https://github.com/OpenVulcan/vulcan-code) 技能系统的运行时，也适用于任何需要受控技能系统、而不是零散嵌入脚本的 AI Agent、IDE、桌面或服务端产品。

## 特色功能

### 🧩 技能运行时

- 在 `ROOT`、`PROJECT`、`USER` 分层根目录中发现、加载、列出、调用和重载技能
- 严格的帮助树，宿主可渲染为文档、工具描述、命令面板或界面面板
- 每个入口一个完整的对象输入 Schema，统一使用 `{skill_id}-{entry_name}` 命名
- 从 GitHub Release 或 URL 安装、更新、卸载，依赖经过校验和验证

### 🧰 标准 Lua API

- `vulcan.fs`、`vulcan.path`、`vulcan.process`、`vulcan.io`、`vulcan.os`、`vulcan.json`、`vulcan.cache`
- `vulcan.context` 提供请求、客户端、预算与技能目录信息；`vulcan.deps` 提供依赖根目录
- `vulcan.host.*` 调用宿主注册的工具；`vulcan.models.*` 使用宿主管理的向量与大模型
- `vulcan.config.*` 为每个技能包提供类型化、可校验的配置，带版本化存储与变更事件

### 🐍 受管 Python 与 Node.js

- 技能通过 `vulcan.runtime.python.*` 与 `vulcan.runtime.node.*` 调用带版本的 Python 与 Node.js 处理器
- 锁定的包依赖、有界输出与确定性的进程树清理
- 严格 System Plugin 租约可使用跨调用的持久会话，配合宿主事件与边沿触发唤醒回调
- 宿主可指定只读发行根与可写环境根

### 🔌 宿主对接

- Rust 宿主直接使用 crate API
- 其他语言使用标准 C ABI 与公共 `_json` FFI
- 公共 `runtime_lease` 与授权绑定的 `system_runtime_lease` 提供持久 Lua VM
- 结构化 `host_result` 返回，首个结果类型是 IDE 级的 `change_set` 编辑
- 可选 SQLite 与 LanceDB，支持动态库、宿主回调或 space controller 模式

### 📦 SDK

- 基于 JSON FFI 的 [TypeScript / Node.js](https://github.com/LuaSkills/luaskills-sdk-typescript)、[Python](https://github.com/LuaSkills/luaskills-sdk-python) 与 [Go](https://github.com/LuaSkills/luaskills-sdk-go) SDK
- 带 SHA-256 校验的运行时资产安装

## LuaSkills 不是什么

- 本身不是 MCP 服务器；MCP 宿主层见 [vulcan-mcp](https://github.com/OpenVulcan/vulcan-mcp)
- 不是宿主配置读取器、预算计算器或界面渲染器
- **不是沙箱。** 技能按受信代码对待。由宿主决定启用哪些根目录、安装哪些技能、暴露哪些管理操作、允许哪些数据库模式，以及每次操作携带的授权。

## 架构

```
┌──────────────────────────────┐         ┌──────────────────────────────────────┐
│  宿主应用                     │  Rust   │  LuaSkills 运行时                     │
│  策略 · 界面 · 权限           │  C ABI  │  技能加载 · 帮助 · 生命周期           │
│  预算 · 模型供应商            │◀───────▶│  LuaJIT VM · 租约 · vulcan.* API      │
│  宿主工具 · 存储              │  JSON   │  受管 Python / Node · 依赖            │
│                              │  FFI    │  配置存储 · SQLite / LanceDB          │
└──────────────────────────────┘         └──────────────────────────────────────┘
                                                          │
                                              skills/<skill_id>/skill.yaml
```

| 目录 | 说明 |
| --- | --- |
| `src/runtime` | 引擎、Lua VM、租约、上下文、帮助、结果、受管运行时 |
| `src/skill` | 清单、根目录、配置与生命周期管理 |
| `src/dependency`、`src/download` | 依赖解析、下载与归档解压 |
| `src/providers` | SQLite 与 LanceDB 绑定 |
| `src/ffi`、`src/ffi_standard`、`src/ffi_embedded` | 公共 `_json` FFI、标准 C ABI 与嵌入式运行时传输 |
| `include/` | FFI 宿主使用的 C 头文件 |
| `contracts/` | 与 SDK 共享的机器契约 |
| `examples/` | Rust、FFI、受管运行时与配置示例 |

## 快速开始

### Rust 宿主

```toml
[dependencies]
luaskills = "0.7.2"
```

[`examples/demo-rust`](examples/demo-rust/README.md) 演示 `call_skill` 与 `vulcan.host.*` 工具桥接。

### 其他语言

| 宿主 | 方式 |
| --- | --- |
| TypeScript / Node.js | `npm install @luaskills/sdk` |
| Python | `pip install luaskills-sdk` |
| Go | `go get github.com/LuaSkills/luaskills-sdk-go` |
| C / C++ | 从 [Releases](https://github.com/LuaSkills/luaskills/releases) 下载 `luaskills-ffi-sdk-{platform}.tar.gz`，使用标准 C ABI |

详见 [SDK 总览](docs/zh-CN/sdk/index.md) 与 [FFI 对接文档](docs/zh-CN/integration/ffi-integration-guide.md)。

### 编写技能

从 [demo-skill](https://github.com/LuaSkills/demo-skill) 开始，再阅读 [Lua Skill 开发手册](docs/zh-CN/development/skill-development.md)。用真实运行时布局调试单个技能：

```bash
cargo run --bin luaskills-debug -- call \
  --runtime-root D:/runtime \
  --skill-path D:/skills/demo-skill \
  --tool ping \
  --args-json "{\"note\":\"hello\"}"
```

`luaskills-debug` 会把技能复制到 `runtime_root/skills/<skill_id>`，再走正常的 `load_from_roots -> call_skill` 路径，使依赖根、状态、数据库与上下文和真实宿主执行一致。它只是仓库内的开发工具，不增加任何 SDK 或 FFI API。

## 文档

| 主题 | 文档 |
| --- | --- |
| 技能开发 | [Lua Skill 开发手册](docs/zh-CN/development/skill-development.md) · [System Plugin 受管运行时使用指南](docs/zh-CN/development/system-plugin-managed-runtime.md) |
| 宿主对接 | [FFI 宿主接入检查清单](docs/zh-CN/integration/ffi-host-checklist.md) · [FFI 对接文档](docs/zh-CN/integration/ffi-integration-guide.md) · [宿主指定受管运行时根目录](docs/zh-CN/integration/managed-runtime-host-roots.md) · [技能包配置](docs/zh-CN/integration/skill-config-system-design.md) · [数据库 Provider](docs/zh-CN/integration/host-database-provider-guide.md) |
| SDK | [SDK 总览](docs/zh-CN/sdk/index.md) |
| 全部 | [中文文档入口](docs/zh-CN/index.md) · [English docs](docs/index.md) |

## 生态仓库

| 仓库 | 说明 |
| --- | --- |
| [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit) | 代码导航、AST 检查、结构搜索、Markdown 导航与安全补丁 |
| [vulcan-curl](https://github.com/LuaSkills/vulcan-curl) | 结构化 HTTP GET / POST 请求 |
| [vulcan-file](https://github.com/LuaSkills/vulcan-file) | 感知忽略规则的列表、精确读取与先预览再修改的小编辑 |
| [vulcan-lua](https://github.com/LuaSkills/vulcan-lua) | 受限的内联或文件式 Lua 执行 |
| [vulcan-testkit](https://github.com/LuaSkills/vulcan-testkit) | 把构建、测试、lint 与类型检查输出整理为紧凑诊断 |
| [vulcan-workmem](https://github.com/LuaSkills/vulcan-workmem) | 项目级工作记忆，用于检查点与交接 |
| [demo-skill](https://github.com/LuaSkills/demo-skill) | 最小技能仓库模板 |
| [luaskills-packages](https://github.com/LuaSkills/luaskills-packages) | Lua 运行时包与原生依赖包 |

## 版本

| 组件 | 版本 |
| --- | --- |
| LuaSkills 核心与 FFI SDK | `0.7.2` |
| TypeScript、Python、Go SDK | 各自独立的版本线，见 [SDK 总览](docs/zh-CN/sdk/index.md#版本规则) |
| Lua 运行时包 | 兼容 `0.1` 系列 |
| 受管 Python / uv | `3.14.6` / `0.11.28` |
| 受管 Node.js / pnpm | `24.18.0` / `11.11.0` |
| 受管运行时默认值 | `workers=4 / idle=60s / sessions=256 / buffer=1MiB/stream / invoke=unlimited` |
| vldb-controller | `0.2.3` |
| vldb-sqlite / vldb-lancedb | `0.1.6` / `0.1.5` |

脚本默认值位于 `scripts/deps/`；除非宿主安装并声明了其他受支持的精确版本，`dependencies.yaml` 必须声明上表中的受管运行时与包管理器版本。发布遵循[发布流程](scripts/release/README.md)：先运行时包，再五平台核心 Release，然后 crate，最后 SDK。

## 参与共建

欢迎提交 Issue 与 Pull Request。

1. Fork 仓库并从 `main` 创建分支。
2. 本地验证：

   ```bash
   cargo fmt --all -- --check
   cargo clippy --all-targets -- -D warnings
   cargo test --all-targets -- --test-threads=1
   cargo deny check
   ```

3. 每个提交都要签署：`git commit -s`，DCO 说明见 [CONTRIBUTING.md](CONTRIBUTING.md)。
4. 每个 PR 聚焦一件事，并说明改了什么、为什么改、如何验证。

## 许可证

[MIT](LICENSE)。第三方声明见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。
