# LuaSkills

[English](README.md) | [简体中文](README.zh-CN.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (BR)](README.pt-BR.md)

[Documentation hub](docs/index.md) | [Skill template](https://github.com/LuaSkills/demo-skill) | [CodeKit example](https://github.com/LuaSkills/vulcan-codekit)

LuaSkills is a Rust-powered runtime for loading, running, and managing Lua-based skills.
It gives host applications a compact way to add scriptable tools, structured help, runtime capabilities, dependency paths, and database-aware skill execution without turning every host into its own plugin runtime.

In one sentence: LuaSkills runs skills; the host decides how those skills become product features.

## What It Is

LuaSkills is the core runtime layer for the LuaSkills ecosystem.
It is designed for applications that want a controlled skill system instead of one-off embedded scripts.

It provides:

- Skill discovery, loading, entry enumeration, and invocation.
- Persistent runtime leases through `runtime_lease` and authority-bound `system_runtime_lease` entrypoints, with strict System Plugin package identity.
- Strict help trees that hosts can render as docs, command palettes, tools, or UI panels.
- Standard Lua capability namespaces under `vulcan.*` and system-side helpers under `vulcan.runtime.*`.
- Runtime context injection for current requests, skill directories, resources, dependency roots, and client metadata.
- Host-owned structured result bridging through `host_result`, including the first canonical `change_set` result kind.
- Optional SQLite and LanceDB bindings for stateful or memory-oriented skills.
- Managed Python and Node.js child runtimes that Lua skills and System Plugins can invoke through `vulcan.runtime.python.*` and `vulcan.runtime.node.*`; persistent cross-eval sessions are restricted to dedicated System Plugin leases.
- Bounded host-side managed-session events with destructive poll/wait APIs and an edge-triggered wake callback that never executes Lua.
- Rust API integration for Rust hosts.
- Standard C ABI and public `_json` FFI for non-Rust hosts.
- SDK-oriented integration paths for TypeScript, Python, and Go.

## What It Is Not

LuaSkills intentionally does not own the whole product surface.

It is not:

- An MCP server by itself.
- A host configuration file reader.
- A client budget calculator.
- A product UI renderer.
- A sandbox boundary for untrusted Lua code.

Hosts stay in charge of policy, authentication, user experience, budgeting, permission prompts, storage placement, and how skills are exposed to users.

## Why Use It

Use LuaSkills when you want skills to feel like a product capability instead of loose scripts.

Good fits include:

- AI agents that need reusable local tools.
- IDEs and developer tools that want scriptable workflows.
- Desktop or server hosts that need first-party and user-installed skills.
- Products that want a stable runtime contract across Rust, C ABI, TypeScript, Python, and Go.
- Memory, database, search, or automation skills that need a clear host ownership model.

LuaSkills is especially useful when you need a split between runtime truth and host presentation:

- The runtime knows how to load and execute skills.
- The host knows what the user is allowed to do.
- The skill author gets stable `vulcan.*` APIs instead of guessing host internals.

## Core Capabilities

| Area | What LuaSkills Provides |
| --- | --- |
| Skill runtime | Load skills, list entries, call entries, reload roots, and manage lifecycle operations. |
| Lua API | Inject `vulcan.call`, `vulcan.fs`, `vulcan.path`, `vulcan.process`, `vulcan.os`, `vulcan.json`, `vulcan.cache`, `vulcan.context`, `vulcan.deps`, `vulcan.sqlite`, `vulcan.lancedb`, and `vulcan.runtime`. |
| Help model | Parse strict skill help trees and expose structured help for host rendering. |
| Host boundary | Keep product policy, UI, budgets, and permissions outside the runtime. |
| Host runtime leases | Support public `runtime_lease` and authority-bound `system_runtime_lease` calls for persistent Lua VM state; System leases require `system_package { id, root, dependencies_file }` and run in a dedicated package-isolated VM. |
| Structured host results | Let hosts opt into `host_result` so skills can return a fourth structured payload such as `change_set` without replacing the main text result. |
| Database providers | Support dynamic-library, host-callback, and space-controller modes for SQLite and LanceDB. |
| Managed child runtimes | Let Lua skills and System Plugins call versioned Python/Node handlers; dedicated System Plugin leases may also open persistent child sessions with locked package dependencies, bounded output, deterministic process-tree cleanup, and host events. |
| Multi-language integration | Expose Rust APIs, standard C ABI, and public `_json` FFI for SDKs and host bridges. |
| Skill roots | Support layered roots such as `ROOT`, `PROJECT`, and `USER` with host-controlled management authority. |

## Ecosystem

LuaSkills is most useful when read together with its ecosystem repositories.

- [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit): a production-grade LuaSkills example that exposes source-code navigation, AST inspection, structural search, Markdown navigation, and safe patching workflows.
- [vulcan-curl](https://github.com/LuaSkills/vulcan-curl): an HTTP request skill built around structured GET / POST entries and curl-style request execution.
- [vulcan-file](https://github.com/LuaSkills/vulcan-file): a focused file operation skill for ignored-aware listing, exact text reads, and preview-first small edits.
- [vulcan-lua](https://github.com/LuaSkills/vulcan-lua): a controlled Lua execution skill for bounded inline code or file-based Lua tasks.
- [vulcan-testkit](https://github.com/LuaSkills/vulcan-testkit): a validation router that turns build, test, lint, and typecheck output into compact diagnostics.
- [vulcan-workmem](https://github.com/LuaSkills/vulcan-workmem): a project-scoped working-memory skill for durable task checkpoints and handoff context.
- [demo-skill](https://github.com/LuaSkills/demo-skill): a minimal skill repository template for learning package layout, `skill.yaml`, runtime entries, and help files.
- [luaskills-sdk-typescript](https://github.com/LuaSkills/luaskills-sdk-typescript): TypeScript and Node.js SDK for the public `_json` FFI path.
- [luaskills-sdk-python](https://github.com/LuaSkills/luaskills-sdk-python): Python SDK for ctypes-based public `_json` FFI integration.
- [luaskills-sdk-go](https://github.com/LuaSkills/luaskills-sdk-go): Go SDK for cgo-backed public `_json` FFI integration.
- [vulcan-mcp](https://github.com/OpenVulcan/vulcan-mcp): MCP host and protocol adaptation layer.

## Documentation

Start here:

- [Documentation hub](docs/index.md): English navigation and product-level map.
- [Chinese documentation](README.zh-CN.md): Chinese product overview and full technical docs entry.
- [Why LuaSkills](docs/product/why-luaskills.md): product narrative, architecture value, and supported integration categories.
- [Skill development manual](docs/skill-development.md): full English manual for skill authors.
- [FFI and SDK overview](docs/ffi/overview.md): English overview for host integrators.
- [System Plugin managed runtime guide](docs/system-plugin-managed-runtime.md): end-to-end package, persistent Python/Node session, event, and cleanup workflow.
- [Database provider overview](docs/providers/database-providers.md): English overview for SQLite and LanceDB ownership.
- [Runtime architecture overview](docs/architecture/runtime-model.md): English overview of host/runtime boundaries.
- [Chinese docs index](docs/zh-CN/index.md): full Chinese technical documentation map.

Important technical docs:

- [Lua Skill developer manual](docs/skill-development.md)
- [Chinese Lua Skill developer manual](docs/zh-CN/skill-development.md)
- [System Plugin managed runtime guide](docs/system-plugin-managed-runtime.md)
- [System Plugin 受管运行时使用指南](docs/zh-CN/system-plugin-managed-runtime.md)
- [FFI integration guide](docs/zh-CN/ffi/integration-guide.md)
- [FFI host checklist](docs/zh-CN/ffi/host-checklist.md)
- [Host database provider guide](docs/zh-CN/providers/host-database-provider-guide.md)
- [Skill root layer policy](docs/zh-CN/architecture/skill-root-layer-policy.md)
- [Skill config system design](docs/zh-CN/architecture/skill-config-system-design.md)
- [Host tooling result bridge and `system_lua_lib` design draft](docs/zh-CN/architecture/host-tooling-result-bridge-design.md)

## Integration Paths

Choose the path by host type:

| Host type | Recommended path |
| --- | --- |
| Rust | Use the Rust crate directly. |
| C / C++ / low-level hosts | Use the standard C ABI. |
| TypeScript / Node.js | Use `luaskills-sdk-typescript` over the public `_json` FFI. |
| Python | Use `luaskills-sdk-python` over the public `_json` FFI. |
| Go | Use `luaskills-sdk-go` or the standard C ABI depending on callback and deployment needs. |
| Mixed host | Use standard C ABI for the stable core path and public `_json` FFI for dynamic operations or SDK-style integration. |

## Quick Start

Rust hosts can depend on the crate directly:

```toml
[dependencies]
luaskills = "0.4"
```

Repository development uses the normal Rust workflow:

```bash
cargo check
cargo test --lib
```

The direct Rust host example lives in [examples/demo-rust](examples/demo-rust/README.md) and covers both `call_skill` and the `vulcan.host.*` host-tool bridge.

For local skill-package debugging, the repository also ships a standalone Rust bin:

```bash
cargo run --bin luaskills-debug -- call \
  --runtime-root D:/runtime \
  --skill-path D:/skills/demo-skill \
  --tool ping \
  --args-json "{\"note\":\"hello\"}"
```

`luaskills-debug` is a repository-side developer tool only. It does not add any SDK API or FFI API. The bin first synchronizes the target skill into `runtime_root/skills/<skill_id>`, then reuses the normal `load_from_roots -> call_skill` path so dependency roots, state directories, databases, and runtime context remain aligned with real host execution. The runtime root is now the single directory input; LuaSkills derives `bin`, `libs`, `lua_packages`, `resources`, `skills`, `temp`, `dependencies`, `state`, `databases`, `config`, and `system_lua_lib` from it. Host tools live directly under `runtime_root/bin`, and native/FFI libraries live under `runtime_root/libs`.

To learn the skill package shape before writing a host integration, start with:

1. [demo-skill](https://github.com/LuaSkills/demo-skill)
2. [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit)
3. [Lua Skill developer manual](docs/skill-development.md)

For FFI hosts, begin with:

1. [FFI beta release notes](docs/zh-CN/ffi/beta-release-notes.md)
2. [FFI host checklist](docs/zh-CN/ffi/host-checklist.md)
3. [FFI integration guide](docs/zh-CN/ffi/integration-guide.md)

## Skill Naming Rules

`skill_id` and every `entry.name` must match `^[a-z]([a-z0-9-]*[a-z0-9])?$`.
The physical skill directory name is the only `skill_id`; `skill.yaml` must not declare a `skill_id` field.
Canonical entries use `{skill_id}-{entry_name}` and may receive a stable `-N` suffix on conflicts.
For GitHub-managed skills, the repository-derived or explicit `skill_id`, release zip prefix, checksum prefix, zip top-level directory, and final installed directory must be identical.
Use `{skill_id}-v{version}-skill.zip`, `{skill_id}-v{version}-checksums.txt`, and a zip containing `{skill_id}/skill.yaml`.

## Trust Model

LuaSkills currently treats skills as trusted code by default.
It does not provide a sandbox security promise for arbitrary untrusted Lua packages.

Product hosts should decide:

- Which roots are enabled.
- Which skills are installed or ignored.
- Which management actions are exposed.
- Which database provider mode is allowed.
- Which user or system authority is attached to an operation.

## Repository Layout

```text
README.md        English product homepage.
README.zh-CN.md  Chinese product homepage.
README.ja.md     Japanese product homepage.
README.ko.md     Korean product homepage.
README.es.md     Spanish product homepage.
README.fr.md     French product homepage.
README.de.md     German product homepage.
README.pt-BR.md  Brazilian Portuguese product homepage.

src/
  dependency/    Skill dependency parsing, installation, and cleanup.
  download/      GitHub, URL, and archive download support.
  host/          Host callbacks and host option models.
  providers/     SQLite and LanceDB provider bindings.
  runtime/       Engine, context, help, result, logging, and cache runtime.
  skill/         Manifest, source records, and lifecycle management.
  ffi.rs         Public `_json` FFI exports.
  ffi_standard.rs Standard C ABI exports.

docs/
  index.md       English documentation hub.
  skill-development.md
                 English skill author overview.
  architecture/  English runtime architecture overview.
  ffi/           English FFI and SDK overview.
  product/       English product-level documents.
  providers/     English database provider overview.
  zh-CN/         Chinese product and deep technical documentation.
  ja/            Japanese product documentation.
  ko/            Korean product documentation.
  es/            Spanish product documentation.
  fr/            French product documentation.
  de/            German product documentation.
  pt-BR/         Brazilian Portuguese product documentation.

examples/
  demo-rust/     Rust host demo with call_skill and host-tool bridge examples.
  demo-ffi/      Packaged FFI demo entry.
  ffi/           C, Python, Go, TypeScript, runtime fixture, and provider demos.
```

## Ecosystem Release Order

For one unified ecosystem release such as `0.5.0`, publish in this order:

1. Release `LuaSkills/luaskills-packages` first so `lua-runtime-packages-*` and `lua-deps-*` already exist for the new compatible series.
2. Release `LuaSkills/luaskills` next, including the crate version plus the main-repo `luaskills-ffi-sdk-*` and demo assets under tag `v0.5.0`.
3. Publish the TypeScript SDK `@luaskills/sdk@0.5.0`.
4. Publish the Python SDK `luaskills-sdk==0.5.0`.
5. Publish the Go SDK module tag `v0.5.0`.
6. Run the **Examples Release** workflow for each SDK only after its package or module tag is already visible upstream.

This order keeps every installer and examples workflow pointed at already-published packages assets, core assets, and SDK packages.

## License

MIT
