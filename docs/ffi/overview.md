# FFI and SDK Overview

[Documentation hub](../index.md) | [Chinese FFI guide](../zh-CN/ffi/integration-guide.md)

LuaSkills exposes the same runtime through several host integration layers.
The goal is to let each host choose the right binding cost without changing the skill model.

## Integration Layers

| Layer | Best For | Notes |
| --- | --- | --- |
| Rust API | Rust hosts | Direct crate integration is the primary path for Rust applications. |
| Standard C ABI | C, C++, low-level hosts, binding generators | Uses explicit structs, buffers, out pointers, status codes, and dedicated free functions. |
| Public `_json` FFI | Dynamic languages and SDKs | Uses JSON input/output envelopes and is easier to wrap from Python, Node.js, TypeScript, and similar hosts. |
| Language SDKs | Product teams that want fewer ABI details | TypeScript, Python, and Go SDKs wrap runtime loading, JSON envelopes, authority helpers, lifecycle calls, and provider callback boundaries. |

## Current Host Integration Highlights

Current stable host-facing features include:

- Public `runtime_lease` endpoints for persistent Lua VM reuse with stable `lease_id + sid + generation` identity.
- Authority-bound `system_runtime_lease` endpoints for package-isolated System Plugins under `system_lua_lib`.
- Public leases accept host-owned `cwd`, `workspace_root`, `lua_roots`, `c_roots`, and `mounts`; strict System leases accept `cwd`, `workspace_root`, `mounts`, and required `system_package { id, root, dependencies_file }`, but reject public `lua_roots` / `c_roots` fields.
- Cross-platform persistent managed Python/Node `session.open(...)` userdata on Windows x86_64, Linux x86_64/aarch64, and macOS x86_64/aarch64 that survives across eval calls on the same lease VM and is deterministically cleaned up on eval rollback, close, replacement, expiration, VM drop, or engine drop; Windows ARM is rejected before resource allocation with `windows_arm_is_not_supported`.
- Independent host-selected read-only distribution and writable environment roots, standard C ABI V3 engine creation, and a read-only JSON FFI runtime resolver. V1/V2 layouts remain unchanged.
- Engine-initialized `managed_runtime_config` for Worker pool capacity/idle retirement, persistent-session capacity/default output buffering, and the omitted `invoke` timeout. Per-call `timeout_ms` and per-session `buffer_limit_bytes` retain explicit priority.
- Engine-level managed-session event poll/wait APIs plus a standard-ABI edge-triggered wake callback; background threads publish events but never call Lua.
- Optional `host_result` bridging so one skill may return `content, overflow_mode, template_hint, host_result`.
- The first canonical structured host result kind: `change_set`, now carrying explicit file lifecycle records plus hunk-level `before + delete[] + insert[] + after` blocks for IDE-grade edit results.

## How To Choose

- Rust host: call the crate directly.
- C or C++ host: start from the standard C ABI.
- TypeScript or Node.js host: prefer [luaskills-sdk-typescript](https://github.com/LuaSkills/luaskills-sdk-typescript).
- Python host: prefer [luaskills-sdk-python](https://github.com/LuaSkills/luaskills-sdk-python).
- Go host: use [luaskills-sdk-go](https://github.com/LuaSkills/luaskills-sdk-go) or standard C ABI depending on deployment and callback needs.
- Mixed host: use standard C ABI for stable core calls and public `_json` FFI for dynamic operations.

## First Integration Sequence

For a new FFI host, stabilize the smallest runtime loop first:

1. `version`
2. `engine_new`
3. `load_from_roots`
4. `list_entries`
5. `call_skill`
6. `run_lua`
7. `engine_free`

After that, add lifecycle operations, query helpers, installation/update flows, provider callbacks, host-tool callbacks, or `space_controller`.

## System Plugin Managed-Session Quick Path

Before creating the engine, fix the managed-runtime resource policy in Rust/JSON host options or the optional standard C ABI V3 `FfiLuaRuntimeManagedRuntimeConfig` pointer. Stable defaults are `4` Workers per exact environment/package-owner pool, `60` idle seconds, `256` persistent sessions per engine, `1 MiB` per session output stream, and no default invoke timeout. All configured numbers must be positive; a null C pointer preserves the complete default policy.

System lease creation is a strict package-bound operation:

```json
{
  "engine_id": 1,
  "sid": "system-plugin/demo",
  "ttl_sec": 0,
  "replace": false,
  "cwd": "D:/runtime/system_lua_lib/demo-plugin",
  "workspace_root": "D:/workspaces/current",
  "mounts": { "project": { "root": "D:/workspaces/current" } },
  "system_package": {
    "id": "demo-plugin",
    "root": "D:/runtime/system_lua_lib/demo-plugin",
    "dependencies_file": "dependencies.yaml"
  },
  "authority": "system"
}
```

`system_package.root` must be an absolute strict descendant of the engine's canonical `system_lua_lib` directory and cannot contain Lua search-path metacharacters. `dependencies_file` is package-relative and must resolve to a contained regular file. `workspace_root`, when present, must be an existing absolute directory; `cwd` resolves under the package root or that authorized workspace. The package root alone is added to the dedicated System VM's Lua module search path and is revalidated before every eval; sibling System Plugins and their managed dependency environments remain isolated.

Managed Node declarations require one exact SemVer and Node.js `22.0.0` or newer; ranges, tags, partial versions, and older releases fail before environment creation. Every live worker or session snapshot retains a shared cross-process environment lifecycle lease. Final publication or replacement attempts a nonblocking exclusive lease and returns a stable busy error while any active worker or session still uses that environment.

Managed Python/Node `status` and pooled `invoke` are supported on Linux, macOS, and Windows; other operating systems return a stable unsupported-platform result. Linux and macOS workers validate relative entries with descriptor-relative no-follow `openat` calls and execute from the pinned snapshot cwd. Windows workers use absolute share-locked snapshot sources plus a short drive or UNC-share root as their neutral cwd. Each Python worker fixes its single snapshot root at the front of `sys.path` and rejects root changes within that worker.

Inside eval, `vulcan.runtime.system_plugin` and `vulcan.runtime.mounts` are recursively read-only userdata views that support indexing, iteration, length, and JSON-result serialization. The three host-owned root fields cannot be replaced. Dedicated System VMs remove global `rawset` and the Lua `debug` library so plugin code cannot bypass those metatables. `vulcan.runtime.workspace_root` is the canonical host-authorized workspace path or `nil`. On Windows x86_64, Linux x86_64/aarch64, and macOS x86_64/aarch64, managed `python.session.open` / `node.session.open` returns process userdata that can be stored in a Lua global and reused by later eval calls on the same `lease_id`; its status includes `managed_session_id` for exact host-event correlation. Managed runtime `status()` exposes the stable `persistent_session` capability object. Windows ARM returns `supported=false` and `reason=windows_arm_is_not_supported` before any environment, snapshot, or process reservation.

System managed sessions emit `stdout_readable`, `stderr_readable`, `exited`, and `failed` events. JSON hosts consume them with strict authority-bound requests:

```json
{
  "engine_id": 1,
  "max_events": 64,
  "timeout_ms": 1000,
  "authority": "delegated_tool"
}
```

Call `luaskills_ffi_managed_session_events_wait_json` with that payload, or omit `timeout_ms` and call `luaskills_ffi_managed_session_events_poll_json`. A successful raw `_json` response uses the standard envelope below:

```json
{
  "ok": true,
  "result": {
    "events": [
      {
        "system_lease_id": "rt_...",
        "sid": "system-plugin/demo",
        "generation": 1,
        "managed_session_id": 7,
        "kind": "stdout_readable",
        "sequence": 12
      }
    ],
    "remaining": 0,
    "timed_out": false
  }
}
```

Poll and wait destructively drain at most positive `max_events` in sequence order. `timed_out` is true only when `wait` reaches its deadline without an event; an ordinary empty `poll` returns `timed_out=false`. Repeated readiness events of the same kind are coalesced per session while output remains bounded in the session buffer.

The wake callback exists only on the standard C ABI as `luaskills_ffi_set_managed_session_wake_callback`. It runs on one serial per-engine background dispatcher and is edge-triggered for a nonempty event queue, so event publishers never execute host code. It must only signal or schedule host work: it must not unwind across the ABI, call Lua, or synchronously re-enter lease evaluation. A nonzero callback result is retried asynchronously with capped exponential backoff while the same queue edge remains pending. The host should subsequently poll/wait events and schedule a normal eval that calls `session:read(...)`. Clear or replace the callback before releasing its `user_data`; the registration call cancels pending retries and waits for retired callback invocations to quiesce.

## Managed Identity Field Quick Path

If an FFI or SDK host projects LuaSkills entries into model-facing or user-facing tools, it should implement the standard `LUASKILL_SID` managed identity contract.

Host-side setup:

1. Inspect each entry input schema after `list_entries`.
2. When a schema contains `LUASKILL_SID` and the host has a stable conversation, task, workspace, or equivalent identity, hide that field from the projected tool schema.
3. Remove the hidden field from the projected `required` list.
4. Inject the stable `LUASKILL_SID` value into the entry arguments before `call_skill`.
   - When the host needs skill-side result hiding or managed-mode detection, wrap the injected identity with the reserved host-managed prefix `LUASKILLS-SID-`.
5. Add managed-mode help text so the model or user does not ask for, print, or save the raw managed identity.
6. Redact or rewrite raw managed identities from projected results when needed.

If the host cannot provide a stable identity, leave `LUASKILL_SID` visible and let the caller or the skill's create/start/bootstrap fallback flow provide it.

## Managed Project-Path Quick Path

If an FFI or SDK host projects LuaSkills entries into model-facing or user-facing tools, it should also implement the conventional `PWD` managed project-path contract when the host has a stable project or workspace context.

Host-side setup:

1. Inspect each entry input schema after `list_entries`.
2. When a schema contains `PWD` and the host has one stable current project/workspace path, hide that field from the projected tool schema.
3. Remove the hidden field from the projected `required` list.
4. Inject the current project/workspace path into `PWD` before `call_skill`.
5. In managed mode, do not ask the model or user to type the project path manually.

If the host cannot provide one stable project/workspace path, leave `PWD` visible and let the caller provide it.

This is a host compatibility convention for better cross-host behavior, not one LuaSkills runtime hard restriction.

## Model Capability Quick Path

Use `vulcan.models.*` when Lua skills need model capabilities that remain fully controlled by the host.
This is different from `vulcan.host.*`: the model surface is fixed and capability-specific, not a generic host tool call.

Host-side setup:

1. Keep provider settings outside LuaSkills, for example in the host's own model configuration file or product settings.
2. Register `luaskills_ffi_set_model_embed_json_callback` only when embeddings are enabled.
3. Register `luaskills_ffi_set_model_llm_json_callback` only when one-turn non-streaming LLM calls are enabled.
4. Create the engine, load roots, and call skills.
5. Clear the process-level callbacks when the host shuts down.

Callback request and response rules:

- Embedding callback request: `{ "text": string, "caller": object }`.
- LLM callback request: `{ "system": string, "user": string, "caller": object }`.
- Embedding success response: `{ "vector": number[], "dimensions": number, "usage"?: object }`.
- LLM success response: `{ "assistant": string, "usage"?: object }`.
- Failure response: `{ "ok": false, "error": { "code": string, "message": string, "provider_message"?: string, "provider_code"?: string, "provider_status"?: number } }`.

`caller` is attached by LuaSkills and may include `skill_id`, `entry_name`, `canonical_tool_name`, `root_name`, `skill_dir`, `client_name`, and `request_id`.
Use it for attribution, budget policy, rate limits, and audit logs.

SDK mapping:

| SDK | Register | Clear |
| --- | --- | --- |
| TypeScript | `setModelEmbedJsonCallback`, `setModelLlmJsonCallback` | `clearModelEmbedJsonCallback`, `clearModelLlmJsonCallback` |
| Python | `set_model_embed_json_callback`, `set_model_llm_json_callback` | `clear_model_embed_json_callback`, `clear_model_llm_json_callback` |
| Go | Typed model callback boundary APIs | Requires a host-owned cgo callback bridge for real registration |

## Key Rules

- Register host callbacks before creating an engine.
- Use `luaskills_ffi_set_host_tool_json_callback` when Lua skills need to call host-registered tools through `vulcan.host.*`.
- Use `luaskills_ffi_set_model_embed_json_callback` and `luaskills_ffi_set_model_llm_json_callback` when Lua skills need host-managed model capabilities through `vulcan.models.*`.
- When projecting entries as tools, follow the `LUASKILL_SID` managed identity contract instead of inventing host-specific session parameter names.
- When projecting entries as tools, follow the conventional `PWD` managed project-path contract instead of forcing every model or user to type host-known project roots manually.
- Do not throw exceptions across C ABI boundaries.
- Do not re-enter the same engine from the same thread.
- Never call Lua or synchronously evaluate a lease from a managed-session wake callback; schedule host work and drain events instead.
- Free owned buffers with the matching LuaSkills free function.
- Let the host decide authority and root write policy.
- Let the host own model provider configuration; LuaSkills only forwards fixed model requests and structured error envelopes.
- Treat the current FFI surface as a controlled host integration contract, not a sandbox boundary.

## Deep References

- [System Plugin managed runtime guide](../system-plugin-managed-runtime.md)
- [System Plugin 受管运行时使用指南](../zh-CN/system-plugin-managed-runtime.md)
- [Host-selected managed runtime roots](../managed-runtime-host-roots.md)
- [宿主指定受管运行时根目录](../zh-CN/managed-runtime-host-roots.md)
- [FFI beta release notes](../zh-CN/ffi/beta-release-notes.md)
- [FFI host checklist](../zh-CN/ffi/host-checklist.md)
- [FFI integration guide](../zh-CN/ffi/integration-guide.md)
- [Host tooling result bridge and `system_lua_lib` design draft](../zh-CN/architecture/host-tooling-result-bridge-design.md)
- [Host database provider guide](../zh-CN/providers/host-database-provider-guide.md)

## Examples

- [C FFI demo](../../examples/ffi/c/README.md)
- [TypeScript FFI demo](../../examples/ffi/typescript/README.md)
- [Standard runtime fixture](../../examples/ffi/standard_runtime/README.md)
- [FFI demo runtime](../../examples/ffi/demo_runtime/README.md)
- [Host provider demo](../../examples/ffi/host_provider_demo/README.md)
