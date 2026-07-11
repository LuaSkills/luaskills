# Managed Runtime Example

This directory contains a minimal LuaSkills package that calls managed Python and Node.js handlers from Lua.
本目录包含一个最小 LuaSkills 包，用于从 Lua 调用受管 Python 与 Node.js handler。

Lua remains the orchestration layer. Python and Node.js run as managed child runtimes, with their own versioned executables, package managers, package environments, and pooled workers.
Lua 仍然是调度层。Python 与 Node.js 作为受管子运行时运行，拥有独立版本的主程序、包管理器、包环境与常驻 worker 池。

This example intentionally covers the ordinary-Skill `status` and pooled `invoke` path. It does not open a persistent `session.open(...)`, because persistent managed sessions are available only inside a strict System Plugin lease. For that end-to-end workflow, see the [System Plugin managed runtime guide](../../docs/system-plugin-managed-runtime.md).
本示例只覆盖普通 Skill 的 `status` 与池化 `invoke` 路径。它不会打开持久 `session.open(...)`，因为持久受管会话只允许在严格 System Plugin 租约内使用。完整流程见 [System Plugin 受管运行时使用指南](../../docs/zh-CN/system-plugin-managed-runtime.md)。

## What This Example Proves

This package validates the full path that a real skill author needs:
该包验证真实 skill 作者需要的完整路径：

- Lua calls `vulcan.runtime.python.invoke(...)` and `vulcan.runtime.node.invoke(...)`.
- Lua 调用 `vulcan.runtime.python.invoke(...)` 与 `vulcan.runtime.node.invoke(...)`。
- Python loads a locked third-party dependency through `uv`.
- Python 通过 `uv` 加载已锁定的第三方依赖。
- Node.js loads locked third-party dependencies through `pnpm`.
- Node.js 通过 `pnpm` 加载已锁定的第三方依赖。
- Node.js native ESM supports default, named, namespace, relative, and side-effect imports.
- Node.js 原生 ESM 支持 default、named、namespace、relative 与 side-effect import。
- The second Python and Node.js calls reuse warm pooled workers.
- 第二次 Python 与 Node.js 调用会复用已预热的常驻 worker。
- Child stdout, stderr, errors, traces, worker reuse state, `env_hash`, and `env_dir` are returned to Lua.
- 子运行时的 stdout、stderr、错误、trace、worker 复用状态、`env_hash` 与 `env_dir` 会准确返回给 Lua。

## Package Layout

```text
managed-child-runtime-debug/
  dependencies.yaml
  skill.yaml
  runtime/smoke.lua
  python/echo.py
  python/requirements.in
  python/requirements.lock
  node/echo.mjs
  node/local-helper.mjs
  node/side-effect.mjs
  node/package.json
  node/pnpm-lock.yaml
```

`dependencies.yaml` declares the child runtime and package manager versions:
`dependencies.yaml` 声明子运行时与包管理器版本：

```yaml
python_runtime:
  version: "3.12.7"
  package_manager: uv
  package_manager_version: "0.11.17"
  lockfile: python/requirements.lock
node_runtime:
  version: "22.11.0"
  package_manager: pnpm
  package_manager_version: "9.15.0"
  package_json: node/package.json
  lockfile: node/pnpm-lock.yaml
```

## Prepare Runtimes

Run the managed runtime fetch script first. This downloads Python, `uv`, Node.js, and `pnpm` into the selected `runtime_root` instead of using system installations.
先运行受管运行时拉取脚本。它会把 Python、`uv`、Node.js 与 `pnpm` 下载到指定 `runtime_root`，而不是使用系统安装。

Windows:
Windows：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/deps/fetch_managed_runtimes.ps1 -RuntimeRoot target/managed-runtime-fetch-check-uv01117-all -Target all -Force
```

Linux, macOS, or WSL:
Linux、macOS 或 WSL：

```bash
RUNTIME_ROOT=target/managed-runtime-fetch-check-uv01117-all scripts/deps/fetch_managed_runtimes.sh all
```

The prepared layout can be checked directly:
可直接检查已准备好的目录布局：

```powershell
python scripts/debug-tools/managed_runtime_layout_check.py target/managed-runtime-fetch-check-uv01117-all
```

```bash
python3 scripts/debug-tools/managed_runtime_layout_check.py target/managed-runtime-fetch-check-uv01117-all
```

## Debug Call

Call the example through the normal `luaskills-debug` path:
通过正式 `luaskills-debug` 路径调用示例：

```powershell
cargo run --bin luaskills-debug -- call --runtime-root target/managed-runtime-fetch-check-uv01117-all --skill-path examples/managed_runtime/managed-child-runtime-debug --tool smoke --args-json '{"text":"debug-call"}' --output content
```

The `smoke` entry returns JSON text containing:
`smoke` 入口会返回 JSON 文本，包含：

- `python_status_before` / `node_status_before`: status before package environments are created.
- `python_status_before` / `node_status_before`：包环境创建前的状态。
- `python_first` / `node_first`: cold calls that create or open the environment and invoke child handlers.
- `python_first` / `node_first`：冷调用，会创建或打开环境并调用子 handler。
- `python_second` / `node_second`: warm calls that should report `worker_reused = true`.
- `python_second` / `node_second`：热调用，通常应返回 `worker_reused = true`。
- `python_status_after` / `node_status_after`: status after the environments are ready.
- `python_status_after` / `node_status_after`：包环境 ready 后的状态。

Each child invoke result includes `ok`, `value`, `stdout`, `stderr`, `error`, `trace`, `status`, `timed_out`, `worker_reused`, `env_hash`, and `env_dir`.
每个子调用结果都包含 `ok`、`value`、`stdout`、`stderr`、`error`、`trace`、`status`、`timed_out`、`worker_reused`、`env_hash` 与 `env_dir`。

## Isolated Smoke Test

Run the isolated smoke script when you want the test to create its own runtime root and fetch dependencies independently.
当希望测试自行创建隔离运行时根目录并独立拉取依赖时，运行隔离冒烟脚本。

Windows:
Windows：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/debug-tools/managed_runtime_smoke.ps1
```

Linux, macOS, or WSL:
Linux、macOS 或 WSL：

```bash
bash scripts/debug-tools/managed_runtime_smoke.sh
```

Use `-SkipFetch` or `--skip-fetch` only when intentionally reusing an existing runtime root during local iteration:
仅在本地迭代时刻意复用已有运行时根目录时使用 `-SkipFetch` 或 `--skip-fetch`：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/debug-tools/managed_runtime_smoke.ps1 -RuntimeRoot target/managed-runtime-fetch-check-uv01117-all -SkipFetch -KeepRuntimeRoot
```

```bash
bash scripts/debug-tools/managed_runtime_smoke.sh --runtime-root target/managed-runtime-fetch-check-uv01117-all --skip-fetch --keep-runtime-root
```

The smoke scripts are repository validation tools. A packaged skill user normally only needs to fetch runtimes once, then call the Lua skill entry normally.
冒烟脚本是仓库验证工具。成型 skill 的用户通常只需要先拉取一次运行时，然后按普通 Lua skill 入口调用即可。
