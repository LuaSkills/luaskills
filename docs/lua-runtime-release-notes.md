## LuaSkills core release packages

### LuaSkills 0.5.7

This patch release fixes configuration watcher routing for atomic file replacement and restores package business validators on Linux ARM64 while retaining their 8 MiB Lua memory limit. It also corrects cross-platform test-only lint and timing issues. The public FFI headers and shared contracts are unchanged from `0.5.6`. The main repository publishes the Rust crate, five-platform FFI SDK, demos, and debug tool; the separate TypeScript, Python, and Go SDK packages remain on their `0.5.5` line. The default LuaSkills tag in this repository's runtime asset sync scripts is now `v0.5.7`.

### LuaSkills 0.5.6

This release adds live host-model availability checks for `vulcan.models.has` and `vulcan.models.status`, owner-aware callback cleanup, and a public runtime-session lease limit constant. It also bounds outbound download requests, hardens Windows descendant-process tests, and includes the repository-wide runtime, dependency, cache, file-watcher, and FFI fixes made since `v0.5.5`. The main repository publishes the Rust crate, FFI SDK, demos, and debug tool; the TypeScript, Python, and Go SDK packages remain on their separately published `0.5.5` line until those repositories are released.

Rust source compatibility note: transaction-internal fields of `PreparedSkillInstall`, `PreparedSkillUpdate`, and `PreparedSkillUninstall` are now crate-private. External Rust callers that accessed or constructed those fields must use supported manager operations and the public `result` field instead. The global Tool Cache configuration entrypoint now reports conflicting registration instead of silently keeping an older configuration.

This Release now publishes only the main-repo artifacts that still belong to `luaskills`: the FFI SDK and the runnable demo packages. Lua runtime packages and native dependency bundles are published separately by [`LuaSkills/luaskills-packages`](https://github.com/LuaSkills/luaskills-packages).

### Assets

- `luaskills-ffi-sdk-{platform}.tar.gz`: FFI SDK package for C ABI or dynamic-library host integration. It contains headers under `include/`, luaskills runtime/import libraries under `lib/`, and the project license.
- `luaskills-demo-ffi-{platform}.tar.gz`: Runnable FFI-mode demo package that shows an external host loading luaskills through the dynamic library. It includes the full `examples/ffi/` tree for C, Go, Python, TypeScript, standard runtime, install smoke tests, and host-provider demos, plus platform-matching runner scripts and dependency fetch scripts.
- `luaskills-demo-rust-{platform}.tar.gz`: Runnable non-FFI Rust demo package that shows a Rust host using the `luaskills` crate. It includes platform-matching runner scripts and dependency fetch scripts.
- `luaskills-debug-tool-{platform}.tar.gz`: Standalone skill-debug workspace. It includes the release-mode `luaskills-debug` binary, a package-local `runtime/`, a `skills/` drop-in directory, and scripts that fetch Lua runtime packages on demand.

Every archive above has a same-name `.sha256` sidecar. LuaSkills `0.5.5` introduced the strict package-level configuration contract, versioned user-level stores, cross-process transactions, cached file-watch reloads, business validators, host events, and matching TypeScript, Python, and Go SDK APIs. That configuration surface intentionally did not retain its earlier draft format or symbols.

### Runtime dependencies

LuaSkills 0.5.1 introduced separate LuaSkills data, read-only managed Python/Node distribution, and writable managed-environment roots. LuaSkills 0.5.5 retains its B3-B7 defaults of `4` Workers per exact environment/package-owner pool, `60` idle seconds, `256` persistent sessions, `1 MiB` per session output stream, and unlimited invoke time. Standard C ABI hosts use `FfiLuaRuntimeHostOptionsV3` plus the optional `FfiLuaRuntimeManagedRuntimeConfig` pointer; JSON FFI and language SDKs use `host_options.managed_runtime_config`.

Demo and debug-tool packages no longer bundle `lua-runtime-{platform}.tar.gz` or `lua-deps-{platform}.tar.gz` from this repository. Instead, their bundled `scripts/deps/fetch_deps.ps1` and `scripts/deps/fetch_deps.sh` scripts download the runtime packages below from `LuaSkills/luaskills-packages`. FFI-mode demo packages additionally bundle `scripts/ffi/fetch_ffi.ps1` or `scripts/ffi/fetch_ffi.sh` for the LuaSkills FFI SDK:

- `lua-runtime-packages-{platform}.tar.gz`: Default Lua runtime package layout containing `lua_packages/`, `libs/`, `resources/`, and `licenses/`.
- `lua-deps-{platform}.tar.gz`: Native dependency bundle used by advanced local builds or other package workflows.

### Demo dependency fetch targets

Demo packages provide standalone dependency upgrade scripts with four targets. The `run` script only runs the demo and does not download dependencies automatically. Windows packages include `upgrade_deps.bat`, `scripts/deps/fetch_deps.ps1`, and `run.ps1`; FFI packages also include `scripts/ffi/fetch_ffi.ps1`. Linux/macOS packages include the matching `.sh` scripts.

SDK repositories additionally publish `scripts/deps/sync_runtime_assets.ps1` and `scripts/deps/sync_runtime_assets.sh` as one direct entrypoint for LuaSkills FFI, Lua runtime packages, and VLDB. Use target `all`, `luaskills`, `lua`, or `vldb`; select `none`, `vldb-controller`, `vldb-direct`, or `host-callback` as the database preset. This repository's default LuaSkills tag is `v0.5.6`; release tags can be overridden explicitly for controlled validation.

- `all`: Fetch `lua-runtime-packages-{platform}.tar.gz`, optional vldb-controller, and the FFI SDK when the package contains `scripts/ffi`.
- `lua`: Fetch `lua-runtime-packages-{platform}.tar.gz` and install it into the demo `runtime/` directory.
- `vldb`: Fetch only vldb-controller and place it under the demo runtime `bin/` directory.
- `ffi`: Fetch only `luaskills-ffi-sdk-{platform}.tar.gz` when the package contains `scripts/ffi`.

In most demo scenarios, run `all` through `upgrade_deps.bat` or `upgrade_deps.sh` first. Use `lua` when you only need to validate Lua package capabilities. Use `vldb` when a runtime already exists and only vldb-controller is missing.

### Debug tool package

The debug tool package is intended for direct skill debugging without a source checkout. Extract `luaskills-debug-tool-{platform}.tar.gz`, run `setup_runtime.ps1` or `setup_runtime.sh` to fetch the `lua` dependency target, place one skill package directory under `skills/`, then run `debug.ps1 inspect`, `debug.ps1 list-tools`, or `debug.ps1 call` on Windows, or the matching `debug.sh` commands on Linux/macOS.

Unlike FFI demo packages, the debug tool does not bundle the extra FFI fetch script. Its `lua` setup still installs the runtime package `lua_packages/`, `libs/`, `resources/`, and `licenses/` directories so Lua C modules can resolve their native dependencies.

The debug binary accepts explicit managed distribution/environment roots and five resource-policy flags for Worker capacity, Worker idle TTL, persistent-session capacity, default per-stream output buffering, and default invoke timeout. Omitted flags preserve the stable engine defaults.

## LuaSkills 主仓库发布资产说明

### LuaSkills 0.5.7

本次补丁发布修复原子替换配置文件时的监听路由问题，并在保持 Lua 8 MiB 内存硬上限的前提下恢复 Linux ARM64 的技能包业务校验；同时修正跨平台测试专用代码的 lint 与时序问题。公开 FFI 头文件和共享契约相对 `0.5.6` 未变。主仓库发布 Rust crate、五平台 FFI SDK、demo 和调试工具；独立的 TypeScript、Python、Go SDK 仍保持 `0.5.5` 版本线。本仓库运行时资产同步脚本的默认 LuaSkills 标签更新为 `v0.5.7`。

### LuaSkills 0.5.6

本次发布新增 `vulcan.models.has` 与 `vulcan.models.status` 的宿主模型实时可用性检查、按所有者清理回调的接口，以及公开的运行时会话租约上限常量；同时为下载请求设置时限，增强 Windows 后代进程测试，并包含 `v0.5.5` 以来运行时、依赖、缓存、文件监听及 FFI 的仓库级修复。主仓库发布 Rust crate、FFI SDK、demo 与调试工具；TypeScript、Python、Go SDK 在各自仓库另行发布前仍保持已发布的 `0.5.5` 版本线。

Rust 源码兼容提醒：`PreparedSkillInstall`、`PreparedSkillUpdate`、`PreparedSkillUninstall` 的事务内部字段现为 crate 私有。外部 Rust 调用方若曾直接读取或构造这些字段，需改用受支持的管理操作与公开的 `result` 字段。全局 Tool Cache 配置入口遇到冲突注册时也不再静默保留旧配置，而会报告冲突。

本 Release 现在只发布仍然属于 `luaskills` 主仓库的核心资产：FFI SDK 与可运行 demo 包。Lua runtime 包和原生依赖包已经拆分到 [`LuaSkills/luaskills-packages`](https://github.com/LuaSkills/luaskills-packages) 独立发布。

### 资产用途

- `luaskills-ffi-sdk-{platform}.tar.gz`：面向 C ABI / 动态库宿主集成的 FFI SDK 包，包含 `include/` 头文件、`lib/` 下的 luaskills 动态库或导入库，以及项目许可证。
- `luaskills-demo-ffi-{platform}.tar.gz`：面向 FFI 模式的可运行 demo 包，演示外部宿主通过动态库加载 luaskills，并携带 `examples/ffi/` 下完整 C、Go、Python、TypeScript、标准 runtime、安装烟测和宿主 provider 示例，以及平台匹配的运行脚本与依赖拉取脚本。
- `luaskills-demo-rust-{platform}.tar.gz`：面向非 FFI / Rust 直连模式的可运行 demo 包，演示 Rust 宿主通过 `luaskills` crate 使用运行时，并携带平台匹配的运行脚本与依赖拉取脚本。
- `luaskills-debug-tool-{platform}.tar.gz`：独立 skill 调试工作台，包含 release 模式的 `luaskills-debug` 二进制、包内 `runtime/`、可直接放 skill 的 `skills/` 目录，以及按需拉取 Lua runtime packages 的脚本。

以上每个归档都有同名 `.sha256` sidecar。LuaSkills `0.5.5` 引入严格的技能包级配置契约、版本化用户级存储、跨进程事务、缓存与文件监听重载、业务校验器、宿主事件，以及匹配的 TypeScript、Python、Go SDK API。该版本有意不保留此前配置草案的格式与符号。

### Runtime 依赖来源

LuaSkills 0.5.1 引入了独立的 LuaSkills 数据根、只读受管 Python/Node 发行根与可写受管环境根。LuaSkills 0.5.5 保留 B3-B7 稳定默认值：每个精确环境/包所有者池 `4` 个 Worker、空闲 `60` 秒、每引擎 `256` 个持久会话、每个 Session 输出流 `1 MiB`，且 invoke 无默认超时。标准 C ABI 宿主使用 `FfiLuaRuntimeHostOptionsV3` 与可选 `FfiLuaRuntimeManagedRuntimeConfig` 指针；JSON FFI 和语言 SDK 使用 `host_options.managed_runtime_config`。

demo 包与 debug-tool 包不再从本仓库发布 `lua-runtime-{platform}.tar.gz` 或 `lua-deps-{platform}.tar.gz`。取而代之，包内自带的 `scripts/deps/fetch_deps.ps1` 与 `scripts/deps/fetch_deps.sh` 会从 `LuaSkills/luaskills-packages` 下载以下资产。FFI 模式 demo 包会额外携带 `scripts/ffi/fetch_ffi.ps1` 或 `scripts/ffi/fetch_ffi.sh` 拉取 LuaSkills FFI SDK：

- `lua-runtime-packages-{platform}.tar.gz`：默认 Lua runtime 目录结构，包含 `lua_packages/`、`libs/`、`resources/` 与 `licenses/`。
- `lua-deps-{platform}.tar.gz`：供高级本地构建或其他 packages 工作流复用的原生依赖包。

### Demo 依赖拉取方式

demo 包内的独立依赖升级脚本支持四个目标。`run` 脚本只负责运行 demo，不会自动下载依赖。Windows 包携带 `upgrade_deps.bat`、`scripts/deps/fetch_deps.ps1` 和 `run.ps1`；FFI 包额外携带 `scripts/ffi/fetch_ffi.ps1`。Linux/macOS 包携带对应的 `.sh` 脚本。

三个 SDK 仓库还会发布 `scripts/deps/sync_runtime_assets.ps1` 与 `scripts/deps/sync_runtime_assets.sh`，作为 LuaSkills FFI、Lua runtime packages 与 VLDB 的统一直接同步入口。目标支持 `all`、`luaskills`、`lua`、`vldb`；数据库预设支持 `none`、`vldb-controller`、`vldb-direct`、`host-callback`。本仓库的默认 LuaSkills 标签为 `v0.5.6`，受控验证时可显式覆盖发布标签。

- `all`：拉取 `lua-runtime-packages-{platform}.tar.gz`、可选 vldb-controller，并在包内存在 `scripts/ffi` 时额外拉取 FFI SDK。
- `lua`：只拉取并安装 `lua-runtime-packages-{platform}.tar.gz` 到 demo 的 `runtime/` 目录。
- `vldb`：只拉取 vldb-controller，并放入 demo runtime 的 `bin/` 目录。
- `ffi`：在包内存在 `scripts/ffi` 时只拉取 `luaskills-ffi-sdk-{platform}.tar.gz`。

一般使用 demo 时先通过 `upgrade_deps.bat` 或 `upgrade_deps.sh` 执行 `all`；只验证 Lua 包能力时执行 `lua`；已有 runtime、只缺 vldb-controller 时执行 `vldb`。

### 调试工具包

调试工具包用于在没有源码仓库的情况下直接调试 skill。解压 `luaskills-debug-tool-{platform}.tar.gz` 后，先运行 `setup_runtime.ps1` 或 `setup_runtime.sh` 拉取 `lua` 依赖目标，再把一个 skill 包目录放到 `skills/` 下，随后在 Windows 上执行 `debug.ps1 inspect`、`debug.ps1 list-tools` 或 `debug.ps1 call`，在 Linux/macOS 上执行对应的 `debug.sh` 命令。

调试二进制支持显式受管发行根/环境根，以及 Worker 容量、Worker 空闲回收、持久会话容量、默认每流输出缓冲与默认 invoke 超时五个资源策略参数；省略时保留稳定引擎默认值。

和 FFI demo 包不同，调试工具包不携带额外的 FFI 拉取脚本。它的 `lua` 初始化仍会安装 runtime package 中的 `lua_packages/`、`libs/`、`resources/` 与 `licenses/` 目录，确保 Lua C module 能解析原生依赖。
