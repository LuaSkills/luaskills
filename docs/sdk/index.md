# LuaSkills SDKs

[Documentation hub](../index.md) | [中文](../zh-CN/sdk/index.md)

The TypeScript, Python, and Go SDKs let non-Rust hosts embed LuaSkills without hand-writing FFI buffers or JSON envelopes. Each SDK lives in its own repository and keeps its own README, examples, and release workflow; this page explains how they relate to the core.

## Packages

| Language | Package | Repository | Requirements |
| --- | --- | --- | --- |
| TypeScript / Node.js | `@luaskills/sdk` (npm) | [luaskills-sdk-typescript](https://github.com/LuaSkills/luaskills-sdk-typescript) | Node.js 24 LTS or newer |
| Python | `luaskills-sdk` (PyPI) | [luaskills-sdk-python](https://github.com/LuaSkills/luaskills-sdk-python) | Python 3.10 or newer |
| Go | `github.com/LuaSkills/luaskills-sdk-go` | [luaskills-sdk-go](https://github.com/LuaSkills/luaskills-sdk-go) | Go 1.22 or newer; cgo for native calls |

```bash
npm install @luaskills/sdk
pip install luaskills-sdk
go get github.com/LuaSkills/luaskills-sdk-go
```

## What the SDKs cover

All three SDKs wrap the public `_json` FFI and share the same host-facing model:

- Native library loading, owned buffers, and engine lifecycle.
- Formal skill roots (`ROOT`, `PROJECT`, `USER`) and authority-aware install, update, enable, disable, and uninstall calls.
- `call_skill`, `run_lua`, public `runtime_lease`, and authority-bound `system_runtime_lease`.
- Package-level skill configuration (`describe`, `get`, `set`, events) generated from `contracts/skill-config/v1/contract.json`.
- Provider callbacks for SQLite, host-tool callbacks for `vulcan.host.*`, and model callbacks for `vulcan.models.*`.
- Host-selected managed runtime roots and the read-only runtime resolver.
- The embedded runtime transport generated from `contracts/embedded/v1/contract.json`.
- Runtime asset installation: `install-runtime` (TypeScript CLI and Python CLI) or the bundled `scripts/deps/sync_runtime_assets.*` scripts (Go).

The Go SDK exposes typed model request, response, and error shapes, but process-level model callback registration still needs a host-owned cgo bridge.

## Versions

- **Core patches keep the contract; contract changes open a minor series.** The embedded contract names its release series (`core_series`, `major.minor`) instead of the exact core version, so its bytes and `contract_sha256` stay the same for every patch of a series. The core release workflow rejects a patch whose contract differs from the previous published release of its series (`scripts/release/contract_series.py`).
- **SDKs follow the core series.** The SDK version is the package version (`package.json`, `pyproject.toml`, Go module tag), mirrored in each repository's `VERSION` file. A new core series ships SDKs with the same `major.minor`; core patches need no SDK release, and an SDK-only fix takes the next patch number of that SDK.
- **Each SDK resolves the newest core patch of its series.** Runtime asset installation (`DEFAULT_LUASKILLS_SERIES` / `DefaultLuaSkillsSeries`) installs the newest published core release of the SDK's series and refuses it unless its packaged contract digest equals the SDK's; there is no fallback to an older patch. The embedded compatibility check accepts any core version of that series with the same contract digest, protocol, ABI and description versions. Hosts can still pin an exact core tag, which is checked the same way.
- **Contracts are copied, never edited.** `contracts/skill-config/v1/` and `contracts/embedded/v1/` are exact copies of the core repository's `contracts/`; generated SDK code is checked against them.
- **Runtime packages are separate.** Lua runtime packages and native dependencies come from [`LuaSkills/luaskills-packages`](https://github.com/LuaSkills/luaskills-packages) in the compatible `0.1` series.

The current series are listed in each SDK README under **Version Alignment**.

## Choosing an integration path

| Host | Recommended path |
| --- | --- |
| Rust | The `luaskills` crate directly; no SDK needed. |
| TypeScript / Node.js | `@luaskills/sdk`. |
| Python | `luaskills-sdk`. |
| Go | `luaskills-sdk-go`, or the standard C ABI when you need full callback control. |
| C / C++ | The standard C ABI; see the [FFI overview](../integration/ffi-overview.md). |

## Release order

SDKs consume published core artifacts, so they release after the core:

1. The matching `LuaSkills/luaskills-packages` series already exists.
2. The core GitHub release (five-platform FFI SDK and demos) is built and verified.
3. The `luaskills` crate is published to crates.io.
4. The TypeScript and Python SDKs are released through their candidate and completion workflows, with cold registry consumer checks.
5. The Go module tag is released after the TypeScript and Python chains pass the Go prerequisite checks.
6. Each SDK runs its **Examples Release** workflow after its package or module tag is visible upstream.
7. [luaskills-debug](https://github.com/LuaSkills/luaskills-debug) is released from its own repository against the published crate.

The exact gates live in [`scripts/release/README.md`](../../scripts/release/README.md) and [`scripts/release/SDK_PREREQUISITES.md`](../../scripts/release/SDK_PREREQUISITES.md).

## Related documents

- [FFI and integration overview](../integration/ffi-overview.md)
- [Runtime architecture](../integration/runtime-model.md)
- [Host-selected managed runtime roots](../integration/managed-runtime-host-roots.md)
- [Package configuration host contract](../integration/package-configuration.md)
- [FFI integration guide (Chinese)](../zh-CN/integration/ffi-integration-guide.md)
