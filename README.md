<div align="center">

# LuaSkills

**A Rust runtime that turns Lua scripts into product-grade skills**

Rust core · LuaJIT skills · Rust API, C ABI, JSON FFI, and TypeScript / Python / Go SDKs

[![crates.io](https://img.shields.io/crates/v/luaskills.svg)](https://crates.io/crates/luaskills)
[![docs.rs](https://img.shields.io/docsrs/luaskills)](https://docs.rs/luaskills)
[![License: MIT](https://img.shields.io/badge/License-MIT-green.svg)](LICENSE)
[![Release](https://img.shields.io/github/v/release/LuaSkills/luaskills)](https://github.com/LuaSkills/luaskills/releases)

**English** | [简体中文](README.zh-CN.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (BR)](README.pt-BR.md)

[Documentation](docs/index.md) · [Skill template](https://github.com/LuaSkills/demo-skill) · [CodeKit example](https://github.com/LuaSkills/vulcan-codekit)

</div>

---

## About

LuaSkills loads, runs, and manages Lua skill packages inside a host application. A host gets scriptable tools, structured help, managed Python and Node.js runtimes, dependency management, and database-aware execution without building its own plugin runtime.

**LuaSkills runs skills; the host decides how they become product features.** Policy, authentication, permission prompts, budgets, storage placement, and user experience stay with the host.

LuaSkills powers the skill system of [Vulcan Code](https://github.com/OpenVulcan/vulcan-code). It fits any AI agent, IDE, or desktop or server product that needs a controlled skill system instead of one-off embedded scripts.

## Highlights

### 🧩 Skill runtime

- Discover, load, list, call, and reload skills across layered `ROOT`, `PROJECT`, and `USER` roots
- Strict help trees that hosts render as docs, tool descriptions, command palettes, or UI panels
- A full object input schema per entry, with canonical `{skill_id}-{entry_name}` naming
- Install, update, and uninstall from GitHub releases or URLs, with checksum-verified dependencies

### 🧰 Standard Lua APIs

- `vulcan.fs`, `vulcan.path`, `vulcan.process`, `vulcan.io`, `vulcan.os`, `vulcan.json`, `vulcan.cache`
- `vulcan.context` for request, client, budget, and skill directory data; `vulcan.deps` for dependency roots
- `vulcan.host.*` calls host-registered tools; `vulcan.models.*` uses host-managed embedding and LLM models
- `vulcan.config.*` gives each package typed, validated configuration with versioned stores and change events

### 🐍 Managed Python and Node.js

- Skills call versioned Python and Node.js handlers through `vulcan.runtime.python.*` and `vulcan.runtime.node.*`
- Locked package dependencies, bounded output, and deterministic process-tree cleanup
- Persistent cross-call sessions for strict System Plugin leases, with host events and an edge-triggered wake callback
- Host-selected read-only distribution roots and writable environment roots

### 🔌 Host integration

- Rust crate API for Rust hosts
- Standard C ABI and public `_json` FFI for every other language
- Public `runtime_lease` and authority-bound `system_runtime_lease` for persistent Lua VMs
- Structured `host_result` returns, starting with IDE-grade `change_set` edits
- Optional SQLite and LanceDB through dynamic libraries, host callbacks, or a space controller

### 📦 SDKs

- [TypeScript / Node.js](https://github.com/LuaSkills/luaskills-sdk-typescript), [Python](https://github.com/LuaSkills/luaskills-sdk-python), and [Go](https://github.com/LuaSkills/luaskills-sdk-go) SDKs over the JSON FFI
- Runtime asset installation with SHA-256 verification

## What LuaSkills is not

- Not an MCP server by itself; [vulcan-mcp](https://github.com/OpenVulcan/vulcan-mcp) provides an MCP host layer
- Not a host configuration reader, a budget calculator, or a UI renderer
- **Not a sandbox.** Skills are trusted code. Hosts decide which roots are enabled, which skills are installed, which management actions are exposed, which database modes are allowed, and which authority each operation carries.

## Architecture

```
┌──────────────────────────────┐         ┌──────────────────────────────────────┐
│  Host application            │  Rust   │  LuaSkills runtime                   │
│  policy · UI · permissions   │  C ABI  │  skill loader · help · lifecycle     │
│  budgets · model providers   │◀───────▶│  LuaJIT VMs · leases · vulcan.* APIs │
│  host tools · storage        │  JSON   │  managed Python / Node · deps        │
│                              │  FFI    │  config stores · SQLite / LanceDB    │
└──────────────────────────────┘         └──────────────────────────────────────┘
                                                          │
                                              skills/<skill_id>/skill.yaml
```

| Directory | Description |
| --- | --- |
| `src/runtime` | Engine, Lua VMs, leases, context, help, results, managed runtimes |
| `src/skill` | Manifests, roots, configuration, and lifecycle management |
| `src/dependency`, `src/download` | Dependency resolution, downloads, and archive extraction |
| `src/providers` | SQLite and LanceDB bindings |
| `src/ffi`, `src/ffi_standard`, `src/ffi_embedded` | Public `_json` FFI, standard C ABI, and embedded runtime transport |
| `include/` | C headers for FFI hosts |
| `contracts/` | Machine contracts shared with the SDKs |
| `examples/` | Rust, FFI, managed-runtime, and configuration examples |

## Getting started

### Rust hosts

```toml
[dependencies]
luaskills = "0.7.2"
```

[`examples/demo-rust`](examples/demo-rust/README.md) shows `call_skill` and the `vulcan.host.*` tool bridge.

### Other languages

| Host | Path |
| --- | --- |
| TypeScript / Node.js | `npm install @luaskills/sdk` |
| Python | `pip install luaskills-sdk` |
| Go | `go get github.com/LuaSkills/luaskills-sdk-go` |
| C / C++ | Download `luaskills-ffi-sdk-{platform}.tar.gz` from [Releases](https://github.com/LuaSkills/luaskills/releases) and use the standard C ABI |

See the [SDK overview](docs/sdk/index.md) and the [FFI overview](docs/integration/ffi-overview.md).

### Writing a skill

Start from [demo-skill](https://github.com/LuaSkills/demo-skill), then read the [skill development manual](docs/development/skill-development.md). To debug one skill against a real runtime layout:

```bash
cargo run --bin luaskills-debug -- call \
  --runtime-root D:/runtime \
  --skill-path D:/skills/demo-skill \
  --tool ping \
  --args-json "{\"note\":\"hello\"}"
```

`luaskills-debug` copies the skill into `runtime_root/skills/<skill_id>` and runs the normal `load_from_roots -> call_skill` path, so dependency roots, state, databases, and context match real host execution. It is a repository developer tool and adds no SDK or FFI API.

## Documentation

| Topic | Documents |
| --- | --- |
| Skill development | [Skill development manual](docs/development/skill-development.md) · [System Plugin managed runtime guide](docs/development/system-plugin-managed-runtime.md) |
| Host integration | [Runtime architecture](docs/integration/runtime-model.md) · [FFI overview](docs/integration/ffi-overview.md) · [Managed runtime roots](docs/integration/managed-runtime-host-roots.md) · [Package configuration](docs/integration/package-configuration.md) · [Database providers](docs/integration/database-providers.md) |
| SDKs | [SDK overview](docs/sdk/index.md) |
| Everything | [Documentation hub](docs/index.md) · [中文文档](docs/zh-CN/index.md) |

## Ecosystem

| Repository | Description |
| --- | --- |
| [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit) | Code navigation, AST inspection, structural search, Markdown navigation, and safe patching |
| [vulcan-curl](https://github.com/LuaSkills/vulcan-curl) | Structured HTTP GET / POST requests |
| [vulcan-file](https://github.com/LuaSkills/vulcan-file) | Ignore-aware listing, exact reads, and preview-first small edits |
| [vulcan-lua](https://github.com/LuaSkills/vulcan-lua) | Bounded inline or file-based Lua execution |
| [vulcan-testkit](https://github.com/LuaSkills/vulcan-testkit) | Turns build, test, lint, and typecheck output into compact diagnostics |
| [vulcan-workmem](https://github.com/LuaSkills/vulcan-workmem) | Project-scoped working memory for checkpoints and handoffs |
| [demo-skill](https://github.com/LuaSkills/demo-skill) | Minimal skill repository template |
| [luaskills-packages](https://github.com/LuaSkills/luaskills-packages) | Lua runtime packages and native dependency bundles |

## Versions

| Component | Version |
| --- | --- |
| LuaSkills core and FFI SDK | `0.7.2` |
| TypeScript, Python, and Go SDKs | Independent version lines; see the [SDK overview](docs/sdk/index.md#versions) |
| Lua runtime packages | compatible `0.1` series |
| Managed Python / uv | `3.14.6` / `0.11.28` |
| Managed Node.js / pnpm | `24.18.0` / `11.11.0` |
| Managed runtime defaults | `workers=4 / idle=60s / sessions=256 / buffer=1MiB/stream / invoke=unlimited` |
| vldb-controller | `0.2.3` |
| vldb-sqlite / vldb-lancedb | `0.1.6` / `0.1.5` |

Script defaults live in `scripts/deps/`; `dependencies.yaml` must declare the exact managed runtime and package-manager versions above unless the host installs and declares another supported version. Releases follow the [release workflow](scripts/release/README.md): runtime packages first, then the five-platform core release, then the crate, then the SDKs.

## Contributing

Issues and pull requests are welcome.

1. Fork the repository and branch from `main`.
2. Verify locally:

   ```bash
   cargo fmt --all -- --check
   cargo clippy --all-targets -- -D warnings
   cargo test --all-targets -- --test-threads=1
   cargo deny check
   ```

3. Sign off every commit with `git commit -s`; see [CONTRIBUTING.md](CONTRIBUTING.md) for the DCO.
4. Keep each pull request focused, and explain what changed, why, and how you verified it.

## License

[MIT](LICENSE). Third-party notices are in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
