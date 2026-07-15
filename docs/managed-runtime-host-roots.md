# Host-Selected Managed Runtime Roots

LuaSkills 0.5.1 separates three filesystem authorities that were previously derived from one `runtime_root`:

- `runtime_root`: LuaSkills data, package stores, snapshots, configuration, and System Plugin state.
- `managed_runtime_distribution_root`: read-only Python, Node.js, uv, and pnpm installations.
- `managed_runtime_environment_root`: writable Python virtual environments and Node dependency environments.

Lua callers cannot override these roots or pass interpreter executables. The host selects them once when it creates the engine.

## Configure an engine

```rust
let mut host_options = LuaRuntimeHostOptions::with_runtime_root(runtime_data_root);
host_options.managed_runtime_distribution_root =
    Some(application_root.join("dependencies/runtimes"));
host_options.managed_runtime_environment_root =
    Some(user_data_root.join("managed-runtime-envs"));
host_options.managed_runtime_config = LuaRuntimeManagedRuntimeConfig {
    worker_pool_max_size_per_environment: 8,
    worker_idle_ttl_secs: 120,
    persistent_session_limit_per_engine: 128,
    persistent_session_default_buffer_limit_bytes_per_stream: 2 * 1024 * 1024,
    invoke_default_timeout_ms: Some(30_000),
};

let engine = LuaEngine::new(LuaEngineOptions::new(pool_config, host_options))?;
```

`runtime_root` remains required when either explicit managed root is configured. Both explicit roots must be absolute. The distribution root must already exist. LuaSkills safely creates the environment root on supported platforms and pins the native filesystem identity of every live root.

When a new field is omitted, LuaSkills preserves the 0.5.0 layout:

```text
distribution_root = <runtime_root>/dependencies/runtimes
environment_root  = <runtime_root>/dependencies/envs
```

The precedence is always `explicit host option > runtime_root-derived default`. Environment variables and `PATH` are never candidates.

## Engine-wide resource policy

`LuaRuntimeHostOptions.managed_runtime_config` fixes the Worker/session policy when the engine is created. Lua code cannot change it, and invalid zero values are rejected before runtime roots, environments, Workers, or sessions are allocated.

| Field | Stable default | Scope |
| --- | ---: | --- |
| `worker_pool_max_size_per_environment` | `4` | Maximum live Workers for one exact environment and package-owner pool key |
| `worker_idle_ttl_secs` | `60` | Idle Worker retirement time in seconds |
| `persistent_session_limit_per_engine` | `256` | Launching plus live persistent Python/Node sessions owned by one engine |
| `persistent_session_default_buffer_limit_bytes_per_stream` | `1048576` (1 MiB) | Default retained bytes for each session stdout or stderr stream |
| `invoke_default_timeout_ms` | `None` / `null` | Default `python.invoke`/`node.invoke` timeout; absence means unlimited |

Every configured numeric value must be greater than zero. A positive per-call `session.open({ buffer_limit_bytes = ... })` overrides the engine buffer default for that session. A positive per-call `invoke({ timeout_ms = ... })` overrides the engine invoke default for that call. Other calls continue to use the immutable engine policy.

## Distribution contract

The configured distribution root points directly at the directory containing `python/` and `node/`:

```text
<distribution_root>/
  python/
    cpython-3.14.6-windows-x64/
      runtime-manifest.json
      python.exe
    uv-0.11.28-windows-x64/
      runtime-manifest.json
      uv.exe
  node/
    node-24.18.0-windows-x64/
      runtime-manifest.json
      node.exe
    pnpm-11.11.0/
      runtime-manifest.json
      bin/pnpm.cjs
```

Every manifest must match the requested runtime, exact version, and platform. Its executable is a safe relative path whose canonical ordinary-file target remains inside the canonical installation directory and configured distribution root. A manifest symlink or an escaping executable symlink is rejected.

Environment identities include SHA-256 hashes of both installation manifests and both executables. Replacing a runtime or package manager under the same version produces a different environment hash; an already resolved stale plan is rejected before use.

## Environment layout

The environment root is used directly:

```text
<environment_root>/
  python/py-3.14.6/<env_hash>/
  node/node-24.18.0/<env_hash>/
```

The marker schema is version 2 and records lock/package metadata plus the four distribution hashes. Worker and persistent-session paths use the same resolved plan, lifecycle lease, environment marker, and package snapshot rules.

## Read-only host resolver

Rust hosts can resolve the same runtime installation without recreating the layout or manifest checks:

```rust
let descriptor = resolve_managed_runtime_install(
    &distribution_root,
    ManagedRuntimeKind::Node,
    "24.18.0",
    "windows-x64",
)?;
```

`ManagedRuntimeInstallDescriptor` returns the canonical install root, canonical executable, exact version/platform, manifest hash, and executable hash.

The Rust descriptor intentionally retains native canonical `PathBuf` values for identity-sensitive host logic. On Windows those internal values may use the `\\?\` form.

The equivalent JSON FFI entrypoint is `luaskills_ffi_managed_runtime_resolve_json`:

```json
{
  "distribution_root": "D:/VulcanCode/dependencies/runtimes",
  "runtime": "node",
  "version": "24.18.0",
  "platform": "windows-x64"
}
```

It returns the normal `{ "ok": true, "result": ... }` envelope and does not create an engine or mutate environment state. JSON input accepts Windows canonical absolute-drive and UNC paths with a `\\?\` / `\\?\UNC\` prefix, but `result.install_root` and `result.executable` always use the equivalent host-visible spelling without that prefix. Other verbatim namespaces are rejected before filesystem lookup.

## C ABI versions

- V1 and V2 layouts are unchanged and use `runtime_root`-derived managed roots.
- `FfiLuaRuntimeHostOptionsV3` embeds the complete V2 value, then adds both managed roots and an optional `FfiLuaRuntimeManagedRuntimeConfig *`.
- A null `managed_runtime_config` pointer preserves all stable defaults. `has_invoke_default_timeout_ms` must be exactly `0` or `1`; when it is `0`, the numeric timeout member is ignored.
- Create V3 engines with `luaskills_ffi_engine_new_v3`.
- JSON engine creation accepts both fields directly inside `host_options`.

Do not enlarge V1 or V2 structs in an existing binding.

## Python, TypeScript, and Go SDKs

All 0.5.1 SDKs expose the same two JSON Host Options and the read-only resolver. Python:

```python
client = LuaSkillsClient(
    runtime_root="D:/VulcanCodeData/luaskills",
    host_options={
        "managed_runtime_distribution_root": "D:/VulcanCode/dependencies/runtimes",
        "managed_runtime_environment_root": "D:/VulcanCodeData/managed-runtime-envs",
        "managed_runtime_config": {
            "worker_pool_max_size_per_environment": 8,
            "worker_idle_ttl_secs": 120,
            "persistent_session_limit_per_engine": 128,
            "persistent_session_default_buffer_limit_bytes_per_stream": 2 * 1024 * 1024,
            "invoke_default_timeout_ms": 30_000,
        },
    },
)
descriptor = LuaSkillsClient.resolve_managed_runtime_install(
    "D:/VulcanCode/dependencies/runtimes",
    "python",
    "3.14.6",
    "windows-x64",
    runtime_root="D:/VulcanCodeData/luaskills",
)
```

TypeScript:

```ts
const client = LuaSkillsClient.create({
  runtimeRoot: "D:/VulcanCodeData/luaskills",
  hostOptions: {
    managed_runtime_distribution_root: "D:/VulcanCode/dependencies/runtimes",
    managed_runtime_environment_root: "D:/VulcanCodeData/managed-runtime-envs",
    managed_runtime_config: {
      worker_pool_max_size_per_environment: 8,
      worker_idle_ttl_secs: 120,
      persistent_session_limit_per_engine: 128,
      persistent_session_default_buffer_limit_bytes_per_stream: 2 * 1024 * 1024,
      invoke_default_timeout_ms: 30_000,
    },
  },
});
const descriptor = LuaSkillsClient.resolveManagedRuntimeInstall({
  runtimeRoot: "D:/VulcanCodeData/luaskills",
  distributionRoot: "D:/VulcanCode/dependencies/runtimes",
  runtime: "node",
  version: "24.18.0",
  platform: "windows-x64",
});
```

Go:

```go
invokeTimeoutMS := uint64(30_000)
hostOptions := map[string]any{
    "managed_runtime_distribution_root": "D:/VulcanCode/dependencies/runtimes",
    "managed_runtime_environment_root":  "D:/VulcanCodeData/managed-runtime-envs",
    "managed_runtime_config": luaskills.ManagedRuntimeConfig{
        WorkerPoolMaxSizePerEnvironment:                   8,
        WorkerIdleTTLSecs:                                 120,
        PersistentSessionLimitPerEngine:                   128,
        PersistentSessionDefaultBufferLimitBytesPerStream: 2 * 1024 * 1024,
        InvokeDefaultTimeoutMS:                            &invokeTimeoutMS,
    },
}
descriptor, err := luaskills.ResolveManagedRuntimeInstall(luaskills.ManagedRuntimeResolveOptions{
    DistributionRoot: "D:/VulcanCode/dependencies/runtimes",
    Runtime:          luaskills.ManagedRuntimeKindNode,
    Version:          "24.18.0",
    Platform:         "windows-x64",
})
```

## Bootstrap and debug tools

The managed runtime fetcher can place interpreter assets directly into an application-owned distribution root while retaining a separate build cache root:

```powershell
scripts/deps/fetch_managed_runtimes.ps1 -RuntimeRoot D:\build-cache -DistributionRoot D:\VulcanCode\dependencies\runtimes -Target all
```

```bash
RUNTIME_ROOT=/tmp/luaskills-build MANAGED_RUNTIME_DISTRIBUTION_ROOT=/opt/vulcancode/dependencies/runtimes scripts/deps/fetch_managed_runtimes.sh all
```

Validate split roots with `managed_runtime_layout_check.py --distribution-root ... --environment-root ...`. The `luaskills-debug` commands accept `--managed-runtime-distribution-root`, `--managed-runtime-environment-root`, and the five `--managed-runtime-*` resource flags shown by `--help`; the managed runtime smoke scripts exercise split roots by default.

## Lua and status behavior

Lua APIs remain unchanged:

```lua
vulcan.runtime.python.status()
vulcan.runtime.python.invoke(...)
vulcan.runtime.python.session.open(...)
vulcan.runtime.node.status()
vulcan.runtime.node.invoke(...)
vulcan.runtime.node.session.open(...)
```

Runtime status includes `distribution_root`, `distribution_source`, `environment_root`, and `environment_source`. Source values are stable: `host_configured` or `runtime_root_default`. Windows status paths are host-visible and never include `\\?\` / `\\?\UNC\`.

## Platforms and failures

Supported native targets are Windows x86_64, Linux x86_64/aarch64, and macOS x86_64/aarch64. Windows ARM/aarch64 and ARM64EC return `windows_arm_is_not_supported`; LuaSkills does not create an environment root or fall back to a system interpreter on those targets.

Typical configuration failures are explicit: relative roots, missing distribution roots, files in place of directories, replaced native filesystem objects, invalid manifests, unsafe executable paths, or changed distribution hashes. Fix the host-owned directory or asset; do not add `PATH` or system-runtime fallbacks.
