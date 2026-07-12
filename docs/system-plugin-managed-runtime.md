# System Plugin Managed Runtime Guide

[简体中文](zh-CN/system-plugin-managed-runtime.md) | [Documentation hub](index.md) | [FFI overview](ffi/overview.md)

This guide is the task-oriented entry point for package-isolated Python/Node child runtimes in a persistent System Plugin lease. It covers the complete path from package preparation to deterministic cleanup. For field-by-field Lua API details, also see the [Skill development manual](skill-development.md#59-managed-python-and-node-child-runtimes).

## 1. Choose The Correct Execution Model

| Requirement | API | Lifetime | Availability |
| --- | --- | --- | --- |
| One structured handler call | `vulcan.runtime.python.invoke(...)` / `node.invoke(...)` | Pooled worker, one request at a time | Ordinary Skill and System Plugin; Linux, macOS, Windows |
| Inspect a declared runtime | `vulcan.runtime.python.status()` / `node.status()` | No child session | Ordinary Skill and System Plugin; Linux, macOS, Windows |
| Keep one stdio process across multiple Lua evaluations | `vulcan.runtime.python.session.open(...)` / `node.session.open(...)` | Bound to one System lease VM | System Plugin only; Windows x86_64, Linux x86_64/aarch64, macOS x86_64/aarch64 |
| Launch an arbitrary executable | `vulcan.process.session.open(...)` | Bound to the current Lua userdata | General process API; not package-managed |

Use a managed persistent session only when the child owns meaningful in-memory state or a long-lived protocol. Prefer pooled `invoke` for independent JSON-compatible calls. Persistent managed sessions are intentionally unavailable to ordinary Skills. Windows ARM/aarch64 is the only explicitly unsupported official target; it fails before environment creation, snapshot creation, or session reservation and never falls back to a system interpreter.

Both `python.status()` and `node.status()` include the same stable machine-readable target capability in every configured, unconfigured, ready, and error response:

```json
{
  "persistent_session": {
    "supported": true,
    "target_os": "macos",
    "target_arch": "aarch64"
  }
}
```

Windows ARM reports `supported: false` with `reason: "windows_arm_is_not_supported"`. Hosts must branch on `supported` and `reason`, not on human-readable error text.

## 2. Prepare The Package

A System Plugin package can use this minimal layout:

```text
<runtime_root>/system_lua_lib/host-indexer/
  dependencies.yaml
  runtime/plugin.lua
  python/sidecar.py
  python/requirements.lock
  node/sidecar.mjs
  node/package.json
  node/pnpm-lock.yaml
```

The package root must be an absolute strict descendant of the engine's canonical `system_lua_lib` directory. It is the only package Lua module root added to the dedicated VM. A sibling package cannot share its `require(...)` scope, dependency identity, worker pool, or persistent session ownership.

Declare only the runtimes the package uses:

```yaml
python_runtime:
version: "3.14.6"
  package_manager: uv
  package_manager_version: "0.11.28"
  lockfile: python/requirements.lock
node_runtime:
  version: "24.18.0"
  package_manager: pnpm
  package_manager_version: "11.11.0"
  package_json: node/package.json
  lockfile: node/pnpm-lock.yaml
```

`node_runtime.version` must be one exact SemVer at or above `22.0.0`. Ranges, tags, partial versions, and older releases are rejected before environment creation. Lockfiles and package metadata are copied to private build inputs and rehashed before the package manager consumes them.

By default, prepare portable runtimes under the same `runtime_root` used to create the engine:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/deps/fetch_managed_runtimes.ps1 -RuntimeRoot <runtime_root> -Target all
```

```bash
RUNTIME_ROOT=<runtime_root> scripts/deps/fetch_managed_runtimes.sh all
```

LuaSkills 0.5.1 also lets the host place distributions and writable environments outside `runtime_root`:

```rust
let mut host_options = LuaRuntimeHostOptions::with_runtime_root(runtime_data_root);
host_options.managed_runtime_distribution_root = Some(application_root.join("dependencies/runtimes"));
host_options.managed_runtime_environment_root = Some(user_data_root.join("managed-runtime-envs"));
```

The fetch scripts accept the exact distribution root without changing the engine's data root:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/deps/fetch_managed_runtimes.ps1 -RuntimeRoot <build_cache_root> -DistributionRoot <distribution_root> -Target all
```

```bash
RUNTIME_ROOT=<build_cache_root> MANAGED_RUNTIME_DISTRIBUTION_ROOT=<distribution_root> scripts/deps/fetch_managed_runtimes.sh all
```

The explicit distribution root must already exist when the engine is created. The explicit environment root is safely created and identity-pinned. See [Host-selected managed runtime roots](managed-runtime-host-roots.md) for Rust, JSON FFI, C ABI V3, hashing, and migration details.

LuaSkills does not fall back to a system Python, system Node.js, external `node_modules`, or undeclared packages.

### Engine initialization policy

The host fixes the following policy in `LuaRuntimeHostOptions.managed_runtime_config` before creating `LuaEngine`; System Plugin Lua code cannot modify it:

| Policy | Default |
| --- | ---: |
| Workers per exact environment/package-owner pool | `4` |
| Worker idle retirement | `60` seconds |
| Persistent sessions per engine | `256` |
| Retained session output per stdout/stderr stream | `1 MiB` |
| `python.invoke` / `node.invoke` timeout | Unlimited |

All configured numeric values must be positive. A per-call positive `invoke.timeout_ms` overrides the engine invoke default, and a per-session positive `session.open.buffer_limit_bytes` overrides the engine output-buffer default. See [Host-selected managed runtime roots](managed-runtime-host-roots.md#engine-wide-resource-policy) for Rust, JSON FFI, standard C ABI V3, and SDK examples.

## 3. Create A Strict System Lease

The public `_json` FFI request includes `engine_id` and a host-injected authority:

```json
{
  "engine_id": 1,
  "sid": "host-indexer/workspace-42",
  "ttl_sec": 0,
  "replace": false,
  "cwd": "D:/runtime/system_lua_lib/host-indexer",
  "workspace_root": "D:/projects/current",
  "mounts": {
    "project": {
      "root": "D:/projects/current"
    }
  },
  "system_package": {
    "id": "host-indexer",
    "root": "D:/runtime/system_lua_lib/host-indexer",
    "dependencies_file": "dependencies.yaml"
  },
  "authority": "delegated_tool"
}
```

Call `luaskills_ffi_system_runtime_lease_create_json` with this request. Accepted authority values are `system` and `delegated_tool`; the host must inject the value rather than forwarding an untrusted caller field.

The System create body is strict:

- `system_package` is required.
- `system_package.root` must be a canonical absolute strict descendant of `system_lua_lib` and cannot contain Lua search-path metacharacters.
- `dependencies_file` is package-relative and must resolve to a contained regular file without a symlink escape.
- `workspace_root`, when supplied, must be an existing absolute directory.
- `cwd` must resolve under the package root or the authorized workspace. It defaults to the package root.
- On Windows, LuaSkills 0.5.2 retains the canonical native object for identity checks and uses the equivalent non-verbatim spelling only when changing the process working directory. Child processes therefore inherit the authorized drive path without `cmd.exe` treating `\\?\` as UNC.
- Public `lua_roots`, `c_roots`, and unknown fields are rejected.

Keep the returned `lease_id`, `sid`, and `generation` together. Send all three on later eval, status, and close requests so a stale or crossed handle fails explicitly.

The standard C ABI function `luaskills_ffi_system_runtime_lease_create(engine_id, request_json, ...)` receives the same strict create body without `engine_id` or `authority` inside `request_json`.

## 4. Open And Reuse A Child Session

The first eval opens the child and stores its userdata in the dedicated lease VM:

```lua
-- Open once and retain the userdata in this lease VM.
-- 仅打开一次，并在当前租约 VM 中保存 userdata。
sidecar = vulcan.runtime.python.session.open({
    file = "python/sidecar.py",
    args = { "--stdio" },
    cwd = "python",
    stdout_encoding = "utf-8",
    stderr_encoding = "utf-8",
    stdin_encoding = "utf-8",
    buffer_limit_bytes = 1024 * 1024,
})

return sidecar:status()
```

For the public `_json` System eval endpoint, the host wraps that Lua source with the lease identity and authority. This compact equivalent is directly serializable:

```json
{
  "engine_id": 1,
  "lease_id": "rt_...",
  "sid": "host-indexer/workspace-42",
  "generation": 1,
  "code": "sidecar = vulcan.runtime.python.session.open({ file = \"python/sidecar.py\", cwd = \"python\", buffer_limit_bytes = 1048576 }); return sidecar:status()",
  "args": {},
  "timeout_ms": 60000,
  "authority": "delegated_tool"
}
```

Send it to `luaskills_ffi_system_runtime_lease_eval_json`. The standard C ABI eval body omits `engine_id` and `authority`, as with create.

For Node.js, change only the API and entry path:

```lua
sidecar = vulcan.runtime.node.session.open({
    file = "node/sidecar.mjs",
    args = { "--stdio" },
    cwd = "node",
    stdout_encoding = "utf-8",
    stderr_encoding = "utf-8",
    stdin_encoding = "utf-8",
    buffer_limit_bytes = 1024 * 1024,
})

return sidecar:status()
```

`session.open(...)` accepts only `file`, `args`, `cwd`, the three stream encodings, and positive `buffer_limit_bytes`. Omitting `buffer_limit_bytes` uses the engine policy (1 MiB per stream by default); an explicit positive value overrides it only for that session. `file` is a package-relative existing source file. Arguments are a dense string array passed directly without shell expansion.

The child runs from a per-session immutable package snapshot and receives controlled package/lease metadata in `LUASKILLS_MANAGED_CONTEXT_JSON`. Python uses the declared managed virtual environment with inherited `PYTHONHOME`, `PYTHONPATH`, and user site-packages removed. Node resolves bare imports from the exact managed `node_modules` environment.

On a later eval of the same `lease_id`, use the stored userdata:

```lua
sidecar:write(vulcan.json.encode(args.request) .. "\n")

local output = sidecar:read({
    timeout_ms = 2000,
    max_bytes = 65536,
    until_text = "\n",
})

return {
    managed_session_id = sidecar:status().managed_session_id,
    stdout = output.stdout,
    stderr = output.stderr,
    timed_out = output.timed_out,
    stdout_total_bytes = output.stdout_total_bytes,
    stdout_dropped_bytes = output.stdout_dropped_bytes,
}
```

The userdata methods are:

| Method | Behavior |
| --- | --- |
| `write(...)` | Encode scalar values with `stdin_encoding`, write to stdin, and flush |
| `read({ timeout_ms?, max_bytes?, until_text? })` | Wait as requested and destructively drain captured output |
| `status()` | Return process state, buffer counters, and the managed-only `managed_session_id` |
| `close({ timeout_ms? })` | Close stdin and wait; terminate the process tree if the timeout expires |
| `kill()` | Immediately terminate the complete process tree |

`read` and `status` expose `stdout_buffered_bytes`, `stdout_total_bytes`, `stdout_dropped_bytes` and the corresponding `stderr_*` fields. A nonzero dropped count means the child produced more data than the configured bounded buffer retained; increase the limit or drain more promptly.

## 5. Consume Host Events

Every System managed session can emit four event kinds:

- `stdout_readable`
- `stderr_readable`
- `exited`
- `failed`

Poll without waiting:

```json
{
  "engine_id": 1,
  "max_events": 64,
  "authority": "delegated_tool"
}
```

Wait for a bounded interval:

```json
{
  "engine_id": 1,
  "max_events": 64,
  "timeout_ms": 1000,
  "authority": "delegated_tool"
}
```

Call `luaskills_ffi_managed_session_events_poll_json` or `luaskills_ffi_managed_session_events_wait_json`. A successful `_json` response wraps this batch in `{"ok":true,"result":...}`:

```json
{
  "events": [
    {
      "system_lease_id": "rt_...",
      "sid": "host-indexer/workspace-42",
      "generation": 1,
      "managed_session_id": 7,
      "kind": "stdout_readable",
      "sequence": 12
    }
  ],
  "remaining": 0,
  "timed_out": false
}
```

Poll and wait are destructive bounded drains. `max_events` must be positive. Events are globally ordered by monotonically increasing `sequence`; `remaining` reports logical events still queued after the drain. Only a wait that reaches its deadline without an event returns `timed_out=true`. Same-kind readiness is coalesced per session, so an event is a signal to read the bounded session buffer, not one message payload.

Correlate by the full event identity:

1. Find the host handle matching `system_lease_id`, `sid`, and `generation`.
2. Schedule a normal System lease eval.
3. Inside that eval, verify/use the stored userdata whose status has the same `managed_session_id`.
4. Call `read(...)` until the required output is drained.

Repository JSON FFI helpers expose the same operations as `ManagedSessionEventsClient.poll(...)` and `.wait(...)` in [Python](../examples/ffi/python/json_runtime.py) and [TypeScript](../examples/ffi/typescript/json_runtime.ts).

## 6. Use The Standard-ABI Wake Callback Safely

`luaskills_ffi_set_managed_session_wake_callback` is available only on the standard C ABI. It reports an empty-to-nonempty queue edge; it does not carry event data and does not replace poll/wait.

The callback runs on one serial background dispatcher per engine. It may signal a condition, post to an event loop, or enqueue host work. It must not call Lua, synchronously evaluate a lease, re-enter callback registration, unwind across the ABI, or release its own `user_data`.

A nonzero callback result is retried asynchronously with capped exponential backoff while the same queue edge remains pending. Event publishers never execute the callback and are not blocked by callback retries.

Before freeing callback-owned state:

1. Call registration with a null callback to clear it.
2. Wait for the clear call to return successfully; it cancels pending retries and waits for retired calls to quiesce.
3. Free `user_data`.

The standard `poll`/`wait` functions return the batch JSON directly through `result_json_out`; `_json` functions return it inside the standard response envelope.

## 7. Close In The Correct Order

Use this normal shutdown sequence:

1. Stop scheduling new application work for the lease.
2. If the child protocol has a graceful shutdown command, send it with `sidecar:write(...)` without closing the userdata.
3. Wait for and drain the final `exited` or `failed` event and any remaining stdout/stderr while the session is still registered.
4. In a lease eval, call `sidecar:close({ timeout_ms = ... })` to finish process-tree and snapshot cleanup.
5. Close the System lease with matching `lease_id`, `sid`, and `generation`.
6. Clear the engine wake callback, if registered.
7. Free the engine.

`close()` and `kill()` unregister the session after process cleanup, so pending events for that session are removed. If the child has no cooperative shutdown command, call `close()` or `kill()` as the authoritative cleanup operation and do not require a later final event. A host that needs the final `exited` edge must observe it before calling either cleanup method.

The public `_json` close request is:

```json
{
  "engine_id": 1,
  "lease_id": "rt_...",
  "sid": "host-indexer/workspace-42",
  "generation": 1,
  "authority": "delegated_tool"
}
```

Send it to `luaskills_ffi_system_runtime_lease_close_json`.

Cleanup is also enforced on failure: a failed eval rolls back every managed session opened by that eval. Lease close, same-SID replacement, expiry, VM destruction, userdata collection, and engine destruction terminate complete descendant process trees. Session snapshots are removed only after process cleanup, and environment lifecycle leases remain held until snapshot cleanup completes.

An unrelated ordinary Skill-root reload does not replace the dedicated System lease manager; a live System session continues until its own lease lifecycle ends.

## 8. Security And Isolation Rules

- `vulcan.runtime.system_plugin` and `vulcan.runtime.mounts` are recursively read-only userdata views; `vulcan.runtime.workspace_root` is the canonical authorized path or `nil`.
- Dedicated System VMs remove global `rawset` and the Lua `debug` library to prevent metatable bypass.
- Package source, dependency manifests, lockfiles, entry files, and authorized cwd objects are validated against fixed filesystem identities. Path traversal, symlink escape, and unsupported package objects are rejected.
- A live worker or session snapshot holds a shared cross-process environment lease. Environment publication/replacement takes a nonblocking exclusive lease and returns a stable busy error instead of racing a live consumer.
- Background output, exit, and failure observers publish bounded engine events and never execute Lua.
- Do not forward caller-provided authority, arbitrary package roots, or arbitrary workspace roots. Those are host policy inputs.

## 9. Troubleshooting

| Symptom | Check | Resolution |
| --- | --- | --- |
| System create rejects `system_package.root` | Root is not a strict canonical descendant of `system_lua_lib`, or contains `?`/`*`-style Lua path metacharacters | Place the package under the engine's derived `system_lua_lib` and pass its canonical absolute path |
| `dependencies_file` is rejected | Path is absolute, escapes with `..`, is a symlink, or is not a regular file | Use a contained package-relative regular file such as `dependencies.yaml` |
| Runtime is configured but unavailable | Portable runtime/package manager was not fetched for the declared exact version | Run the managed runtime fetch script against the selected distribution root and verify `distribution_source` |
| Node version is rejected | Version is a range/tag/partial version or below `22.0.0` | Pin one exact supported SemVer |
| `session.open(...)` reports `windows_arm_is_not_supported` | Host target is Windows ARM/aarch64 | Do not create a persistent session and do not fall back to a system interpreter; use a supported native target |
| `session.open(...)` is denied in a Skill | Persistent managed sessions require a dedicated System package context | Use `invoke` in the Skill, or move the long-lived sidecar to an authorized System Plugin |
| Environment publication reports busy | A worker or session snapshot still holds a lifecycle lease | Close owning leases/sessions and retry; do not delete or replace the environment manually |
| Events arrive but output appears empty | Another read drained the buffer, readiness was coalesced, or output was dropped by the bound | Serialize reads per session and inspect `*_buffered_bytes` / `*_dropped_bytes` |
| A handle reports SID/generation mismatch | Host mixed identities or reused a stale handle after replacement | Rebuild the handle from the latest create/list result and carry all identity guards |
| Wake callback repeats | The queue edge remains pending, or the callback returned nonzero | Drain events in host work and return zero after successful scheduling |

## 10. Release Checklist

- The engine is created with the intended data, distribution, environment roots, and B3-B7 resource policy; status reports the expected `host_configured` or `runtime_root_default` sources.
- Every System package has a unique id, strict root, contained dependency manifest, and locked dependencies.
- Hosts inject authority and keep `lease_id + sid + generation` as one handle.
- Persistent sessions are exercised natively on Windows x86_64, Linux x86_64/aarch64, and macOS x86_64/aarch64; Windows ARM is rejected by structured capability.
- The host drains events with a positive bound and never treats readiness as output content.
- The wake callback only schedules work and is cleared before its state is freed.
- Shutdown tests observe final events before explicit close when required and cover close, kill, replacement, expiry, failed eval, engine destruction, descendants, snapshots, and output-buffer drops.

## 11. Related References

- [FFI and SDK overview](ffi/overview.md)
- [Chinese FFI integration guide](zh-CN/ffi/integration-guide.md)
- [Skill development manual](skill-development.md#59-managed-python-and-node-child-runtimes)
- [Managed runtime invoke example](../examples/managed_runtime/README.md)
- [Standard C ABI header](../include/luaskills_ffi.h)
- [Public JSON FFI header](../include/luaskills_json_ffi.h)
