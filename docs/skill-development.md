# Lua Skill Developer Manual

[Documentation hub](index.md) | [Chinese manual](zh-CN/skill-development.md)

## 1. Purpose

This manual is for **Lua Skill authors**. It documents the `vulcan.*` capability surface that the current runtime actually injects into Lua, including:

- Which `vulcan.*` APIs are available.
- How each API is called.
- Which APIs are always available and which ones depend on host or current-skill binding state.
- Which fields are implementation details and should not be treated as long-term public contracts by skills.

This document follows the current implementation, especially the Lua injection logic in `src/runtime/engine.rs`.

## 2. Quick Summary

The current skill runtime exposes these top-level capabilities:

- `vulcan.call`
- `vulcan.runtime.*`
- `vulcan.fs.*`
- `vulcan.path.*`
- `vulcan.process.*`
- `vulcan.os.*`
- `vulcan.json.*`
- `vulcan.cache.*`
- `vulcan.host.*`
- `vulcan.context.*`
- `vulcan.deps.*`
- `vulcan.sqlite.*`
- `vulcan.lancedb.*`

Three important facts:

1. The runtime currently treats skills as **trusted code** by default and does not provide a sandbox security promise.
2. Some values under `vulcan.context.*`, `vulcan.deps.*`, `vulcan.sqlite.*`, and `vulcan.lancedb.*` depend on host injection or current-skill binding state. Do not assume they are always valid.
3. Hosts can force-skip skills through `LuaRuntimeHostOptions.ignored_skill_ids`. Ignored skills do not prepare dependencies, bind databases, or register entries.

## 2.1 Skill Naming And Release Package Rules

`skill_id` is the stable primary key shared by the LuaSkills runtime, lifecycle management, config namespaces, dependency folders, database bindings, and canonical entry names. The current rule is "the directory name is the `skill_id`"; `skill_id` is not declared by a field in `skill.yaml`.

`skill_id` and every `entry.name` must use the same identifier format:

```text
^[a-z]([a-z0-9-]*[a-z0-9])?$
```

Meaning:

- Start with a lowercase ASCII letter.
- Continue only with lowercase ASCII letters, digits, and hyphens `-`.
- Do not start with a digit, use uppercase letters or underscores, or end with a hyphen.
- Valid examples: `vulcan-codekit`, `codekit2`, `vulcan-runtime-tools`.
- Invalid examples: `2codekit`, `Vulcan-codekit`, `vulcan_codekit`, `vulcan-codekit-`.

Skill package layout rules:

- The physical directory name is the final `skill_id`. For example, `skills/vulcan-codekit/` has `skill_id = vulcan-codekit`.
- `skill.yaml` must not declare a `skill_id` field. The runtime rejects a skill that declares it.
- `skill.yaml` `name` is human-readable metadata and does not participate in `skill_id` matching.
- Each `entries[].name` is a local entry name inside the current skill and must also satisfy the identifier rule above.
- Runtime canonical entry names are exposed as `{skill_id}-{entry_name}`. If a name conflicts with a host-reserved name or another entry, the runtime appends a stable numeric suffix, producing `{skill_id}-{entry_name}-{N}`.

GitHub-managed skill installs and release assets must use the same `skill_id`:

- If an install request does not pass an explicit `skill_id`, the runtime derives it from the GitHub repository name. It does not strip a `luaskills-` prefix automatically.
- The release zip file name must be `{skill_id}-v{version}-skill.zip`.
- The checksum file name must be `{skill_id}-v{version}-checksums.txt`.
- The zip must contain only one top-level directory named exactly `{skill_id}`, and it must contain `{skill_id}/skill.yaml`.
- Repository name, release asset prefix, checksum asset prefix, zip top-level directory, and final install directory should all use the same `skill_id`.

## 2.2 Entry Input Schema

LuaSkills now supports one full AI-facing object input schema per entry.

Recommended rule:

- Keep `skill.yaml` for human-friendly metadata such as `name`, `version`, `lua_entry`, and help links.
- Put complex tool input schemas in external JSON files under `schemas/`.
- Use `input_schema_file` when the schema contains nested objects, arrays with `items`, `oneOf` / `anyOf`, or strict `additionalProperties` rules.
- Keep legacy `parameters` only for backward compatibility or simple flat entries.

Current entry-schema fields:

- `parameters`: legacy flat parameter list. When `input_schema` and `input_schema_file` are both absent, the runtime projects `parameters` into one object schema automatically.
- `input_schema`: optional inline object schema inside `skill.yaml`.
- `input_schema_file`: optional relative JSON file path under `schemas/`. This is the recommended format for non-trivial schemas.

Rules:

- The final entry input schema must be one JSON object schema whose root `type` is `object`.
- `input_schema` and `input_schema_file` must not be declared together on the same entry.
- `input_schema_file` must stay under `schemas/` and must be one relative path.
- When `parameters` is empty but a full schema is present, LuaSkills derives one legacy top-level parameter preview from root `properties` for compatibility exports.

Example:

```yaml
entries:
  - name: node_source
    description: Read selected nodes.
    lua_entry: runtime/node_source.lua
    lua_module: demo.node_source
    input_schema_file: schemas/node_source.input.schema.json
```

```json
{
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "nodes": {
      "type": "array",
      "items": {
        "type": "object",
        "properties": {
          "file": { "type": "string" },
          "structural_path": { "type": "string" }
        },
        "required": ["file", "structural_path"]
      }
    }
  },
  "required": ["nodes"]
}
```

## 2.3 Managed Identity Field Contract

Some skills need to bind multiple entry calls to the same session, task, or context state. LuaSkills reserves `LUASKILL_SID` as the standard managed identity field for skill entry arguments.

This is a LuaSkills ecosystem contract, not an MCP-specific convention. The runtime does not treat `LUASKILL_SID` as a magic field; skill code receives it as an ordinary string argument. Hosts and adapters that project LuaSkills entries into user-facing or model-facing tools are responsible for deciding whether the field is visible, hidden, or automatically injected.

Skill author rules:

- New skills that need a stable session, task, or context identity should use the entry argument name `LUASKILL_SID`.
- The field is only an identity handle for state continuity. Do not use it as an auth token, secret, database credential, or permission boundary.
- Skills must not assume every host can provide a managed identity. Help text should describe both managed and non-managed behavior.
- A stateful skill should provide a create, start, open, or bootstrap style entry when it can establish or resume state.
- If the caller explicitly passes `LUASKILL_SID`, the skill should reuse it instead of generating a new identity.
- If `LUASKILL_SID` is omitted and the skill supports non-managed fallback, the create/start/bootstrap entry may generate a new public identity.
- When a skill generates a public identity, the result must visibly return that identity and state that later calls should pass the same value.
- A skill may suggest saving a generated public identity into a host- or project-approved rules location, but it must not write that location without explicit user or host authorization.
- Non-create entries that require `LUASKILL_SID` should return a clear error when it is missing, including the expected create/start/bootstrap recovery path.

Host and adapter rules:

- When exposing entry schemas, scan for an input property named `LUASKILL_SID`.
- If the host has a stable managed identity for the current conversation, task, workspace, or equivalent context, it may hide `LUASKILL_SID` from the model/user-facing schema and remove it from the visible `required` list.
- Before invoking the entry in managed mode, the host must inject the hidden `LUASKILL_SID` into the argument object.
- When the host wants skills to detect and hide host-managed identities in tool results, it should inject the value with the reserved prefix `LUASKILLS-SID-`.
- The injected value must remain stable for the intended host scope and must not be freshly randomized on every call.
- If the host cannot provide a stable managed identity, it should leave `LUASKILL_SID` visible and let the caller or the skill fallback flow handle it.
- Managed-mode help should tell the model or user that the identity is injected by the host and should not be requested, printed, or saved.
- If a tool result includes the injected raw managed identity, the host should redact it or rewrite it into a managed-state note before returning it to the model/user.

These rules apply to every projection layer, including MCP, gRPC, FFI/SDK hosts, IDE integrations, and embedded product hosts.

## 2.4 Managed Project-Path Field Contract

Some skills need one caller-visible project or workspace path that follows the host's active project scope instead of the raw runtime process working directory. LuaSkills reserves `PWD` as the conventional entry-argument name for that purpose.

This is a LuaSkills ecosystem compatibility convention, not an authority-bearing magic field. The runtime recognizes `PWD` only to normalize its path spelling at the Lua boundary. Hosts and adapters that project LuaSkills entries into user-facing or model-facing tools decide whether the field stays visible, hidden, or automatically injected.

Skill author rules:

- New skills that need one host- or caller-provided project/workspace root should use the entry argument name `PWD`.
- Treat `PWD` as one portability-oriented project/workspace path contract, not as proof of authority, one sandbox boundary, or a guarantee that the runtime process itself is currently inside that directory.
- Skills should prefer explicit `PWD` over `vulcan.runtime.cwd()` when behavior must follow the host's active project/workspace scope rather than the runtime process directory.
- Skills must not assume every host can inject `PWD`; help text should describe both managed and non-managed behavior.
- If the caller explicitly passes `PWD`, the skill should reuse it rather than overwrite it with one guessed path.
- If `PWD` is omitted and the skill has a safe fallback, the skill may fall back to `vulcan.runtime.cwd()` or another documented project-resolution path, but that fallback should be explicit in help and behavior.

Host and adapter rules:

- When exposing entry schemas, scan for an input property named `PWD`.
- If the host has a stable current project, workspace root, or equivalent path context, it should hide `PWD` from the model/user-facing schema and remove it from the visible `required` list.
- Before invoking the entry in managed mode, the host should inject the current project/workspace path into `PWD`.
- In managed mode, the host should supply the real project/workspace path directly instead of asking the model or user to provide it.
- If the host cannot provide a stable current project/workspace path, it should leave `PWD` visible and let the caller provide it.
- This is a host compatibility convention for better cross-host behavior, not a hard LuaSkills runtime restriction. Hosts may adopt it progressively.

These rules apply to every projection layer, including MCP, gRPC, FFI/SDK hosts, IDE integrations, and embedded product hosts.

On Windows, a host may obtain a canonical path in `\\?\C:\...` or `\\?\UNC\server\share\...` form. LuaSkills removes that verbatim prefix from the reserved top-level `PWD` argument before it enters Lua. The same boundary rule applies to declared runtime path fields; arbitrary JSON strings are not rewritten merely because they resemble paths. Only absolute drive and UNC verbatim forms have proven ordinary equivalents. Volume GUID, device, pipe, mixed-separator, and other verbatim namespaces are rejected explicitly instead of being reinterpreted as relative paths. Prefix cleanup is a compatibility transformation, not authorization or canonicalization.

## 3. Top-Level Capability Overview

| Top-level item | Purpose | Available by default | Notes |
| --- | --- | --- | --- |
| `vulcan.call` | Call another skill entry | Yes | The second argument must be a Lua table |
| `vulcan.runtime` | Runtime helper capabilities | Yes | Includes logging, cwd, luaexec, skill management bridge, and more |
| `vulcan.fs` | File system reads and writes | Yes | No sandbox restriction is provided |
| `vulcan.io` | Rust-managed IO | Yes | Supports encoding-aware file IO, managed `popen`, and luaexec `io` interception |
| `vulcan.path` | Path joining | Yes | Returns Lua-friendly system paths |
| `vulcan.process` | Start child processes | Yes | Includes one-shot `exec` and interactive `session` |
| `vulcan.os` | Host OS and architecture info | Yes | `os`, `arch` |
| `vulcan.json` | JSON encode/decode | Yes | Type-preserving JSON/Lua conversion |
| `vulcan.cache` | Runtime cache | Yes | Disabled inside `vulcan.runtime.lua.exec` |
| `vulcan.models` | Standard model capabilities | Yes | Capability is active only when the host registers the matching callback |
| `vulcan.host` | Host-registered tool bridge | Yes | Empty until the host registers a callback |
| `vulcan.context` | Request and current-entry context | Yes | Most values are host-injected |
| `vulcan.deps` | Current skill dependency root paths | Yes | May be `nil` without a resolved skill context |
| `vulcan.sqlite` | Current skill SQLite binding | Conditional | `enabled/status/info` still exist when disabled |
| `vulcan.lancedb` | Current skill LanceDB binding | Conditional | `enabled/status/info` still exist when disabled |

## 3.1 Managed IO And Process Encoding

`vulcan.io` is the Rust-managed IO surface for AI-generated code, cross-platform file operations, and `luaexec` isolated execution. For Windows Unicode paths, explicit text decoding, and `io.popen` output handling, prefer `vulcan.io` over native `io` or `os`.

### 3.1.1 `vulcan.io.*` API Reference

| API | Parameters And Types | Return Value | Notes |
| --- | --- | --- | --- |
| `vulcan.io.open(path, mode?, options?)` | `path: string`; `mode?: string`, default `r`; `options?: string \| { encoding?: string }` | Managed file handle | Supports `r`, `w`, `a`, `rb`, `wb`, `ab`, `r+`, `w+`, `a+`, and binary/update combinations |
| `vulcan.io.read_text(path, options?)` | `path: string`; `options?: string \| { encoding?: string }` | `string` | Reads one whole text file |
| `vulcan.io.write_text(path, content, options?)` | `path: string`; `content: string`; `options?: string \| { encoding?: string }` | `true` | Overwrites one whole text file; does not create parent directories |
| `vulcan.io.append_text(path, content, options?)` | same as above | `true` | Appends text |
| `vulcan.io.lines(path, options?)` | `path: string`; `options?: string \| { encoding?: string }` | iterator function | Yields one line at a time and returns `nil` at EOF |
| `vulcan.io.popen(command, mode?, options?)` | `command: string`; `mode?: string`, default `r`; `options?: string \| { encoding?: string, timeout_ms?: integer }` | Managed file handle | Read mode only for now; write mode is not implemented |
| `vulcan.io.tmpfile()` | none | Managed file handle | Creates one temporary file that is removed on close |

### 3.1.2 Managed File Handle Methods

The handles returned by `vulcan.io.open(...)`, `vulcan.io.popen(...)`, and `vulcan.io.tmpfile()` support the same methods:

| Method | Parameters And Types | Return Value | Notes |
| --- | --- | --- | --- |
| `file:read(...)` | no args, `"*a"` / `"a"`, `"*l"` / `"l"`, or positive integer length | `string`, `nil`, or multiple values | Text mode returns decoded UTF-8 text; binary mode returns raw Lua byte strings |
| `file:write(...)` | one or more Lua scalar values | `true` | Text mode encodes text; binary mode writes raw bytes |
| `file:flush()` | none | `true` | Flushes pending writes |
| `file:close()` | none | `boolean` | Regular files usually return `true`; `popen` handles return child success |
| `file:seek(whence?, offset?)` | `whence?: "set" \| "cur" \| "end"`; `offset?: integer` | `integer` | Returns the new cursor offset |
| `file:lines()` | none | iterator function | Iterates from the current cursor until EOF |
| `file:setvbuf(...)` | any args | `true` | Compatibility no-op |

Extra rules:

- `file:read()` with no arguments reads one line without the trailing newline.
- `file:read(0)` returns an empty string, not `nil`.
- `file:read(n)` returns `nil` after EOF.
- `file:seek(...)` clamps beyond EOF to the buffer end and errors when moving before the start.
- In text mode, strings passed to `file:write(...)` must be valid UTF-8. Use binary mode or `vulcan.fs.read_bytes/write_bytes` when you need raw bytes.

### 3.1.3 Encoding Options

`options.encoding` or `encoding` currently supports:

| Value | Meaning |
| --- | --- |
| `utf-8` | Standard UTF-8 text |
| `system` | Host default text encoding; Windows ANSI code page on Windows |
| `oem` | Windows console OEM code page |
| `gbk` | GBK |
| `gb18030` | GB18030 |
| `latin1` | Latin-1 / ISO-8859-1 |
| `base64` | Byte-preserving Base64 text transport |

Default encoding selection:

- Use `LuaRuntimeHostOptions.default_text_encoding` when the host configured it.
- Otherwise use:
  - `system` on Windows
  - `utf-8` on other platforms

### 3.1.4 Managed `io.*` Compatibility Layer

Inside `vulcan.runtime.lua.exec(...)`, common `io.*` calls are redirected to the managed compatibility layer:

- `io.open`
- `io.input`
- `io.output`
- `io.read`
- `io.write`
- `io.flush`
- `io.close`
- `io.lines`
- `io.popen`
- `io.tmpfile`
- `io.type`

Important behavior:

- `io.input(path_or_file)` and `io.output(path_or_file)` can set default handles.
- When no explicit `io.output(...)` is configured, `io.write(...)` writes to runtime logging instead of silently disappearing.
- `io.close()` with no argument closes the current default output handle.
- `io.type(value)` returns `"file"`, `"closed file"`, or `"nil"`.
- Hosts can disable this replacement with `LuaRuntimeHostOptions.capabilities.enable_managed_io_compat = false`. `vulcan.io` itself still stays available.

### 3.1.5 Examples

Text read:

```lua
local text = vulcan.io.read_text("D:/data/example.txt", {
    encoding = "utf-8",
})

return text
```

Binary handle read:

```lua
local file = vulcan.io.open("D:/data/archive.bin", "rb")
local bytes = file:read("*a")
file:close()

return #bytes
```

Managed `popen`:

```lua
local file = vulcan.io.popen("cmd /C dir", "r", {
    encoding = "oem",
    timeout_ms = 3000,
})

local output = file:read("*a")
local ok = file:close()

return vulcan.json.encode({
    ok = ok,
    output = output,
})
```

## 3.2 Host-Forced Skill Ignore List

`ignored_skill_ids` is a host runtime policy used to skip selected skills early during load.

Typical scenarios:

- The host already provides a stronger native, gRPC, or VMM implementation.
- A default skill package overlaps with existing host functionality.
- A database-backed skill has been replaced by a host-side service, so SQLite or LanceDB binding should not start.

Matching rules:

- It matches the `skill_id` derived from the skill directory.
- It does not match `skill.yaml` `name`.
- It is not declared by the skill itself and is not part of dependency resolution.

Runtime effects:

- The whole skill is skipped when the ignore list matches.
- `dependencies.yaml` is not prepared.
- SQLite and LanceDB are not bound.
- No entries are registered.
- The skill does not appear in `list_entries` and cannot be called through `vulcan.call`.

This capability preserves the host and user's final choice. If users should still be able to use a skill, hosts should not add it to the ignore policy.

## 4. `vulcan.call`

### 4.1 Purpose

`vulcan.call(name, args)` lets one skill call another loaded skill entry.

- `name`: target entry canonical name.
- `args`: must be a Lua table.
- Return value: directly forwards the target skill's return values and supports multiple returns.

### 4.2 Minimal Example

```lua
local ok, result = pcall(vulcan.call, "demo-skill-run", {
    query = "hello",
    limit = 5,
})

if not ok then
    vulcan.runtime.log("warn", "call failed: " .. tostring(result))
    return nil
end

return result
```

### 4.3 Notes

- Missing entries raise an error directly.
- `args` must be a table, not a string or another scalar.
- `vulcan.call` inherits the current request context, budget snapshot, and tool config, then switches to the target skill's file context and database bindings.
- In `luaexec` scenarios, extra reentry protection prevents unbounded recursion back into the current runtime caller.

## 4.5 `vulcan.models.*`

`vulcan.models.*` is the fixed standard model capability surface for Lua skills.
It is not a generic host-tool call and does not let Lua choose provider configuration.

### 4.5.1 API Reference

| API | Parameters And Types | Return Value | Notes |
| --- | --- | --- | --- |
| `vulcan.models.status()` | none | `{ ok = true, capabilities = { embed = boolean, llm = boolean } }` | Always present; only reports whether the host registered each capability |
| `vulcan.models.has(capability)` | `capability: string` | `boolean` | Only recognizes `embed` and `llm` |
| `vulcan.models.embed(text)` | `text: string`, non-empty | success or failure envelope | Runs single-text embedding |
| `vulcan.models.llm(system, user)` | `system: string`, `user: string`, both non-empty | success or failure envelope | Runs one non-streaming LLM turn |

Minimal example:

```lua
if not vulcan.models.has("embed") then
    return {
        ok = false,
        reason = "model-embed-unavailable",
    }
end

local result = vulcan.models.embed("hello")
if not result.ok then
    return result
end

return result.vector
```

### 4.5.2 Result Shapes

`embed(...)` success envelope:

```lua
{
    ok = true,
    vector = { 0.1, 0.2, 0.3 },
    dimensions = 1536,
    usage = {
        input_tokens = 123,
    },
}
```

Field notes:

| Field | Type | Meaning |
| --- | --- | --- |
| `ok` | `boolean` | `true` on success |
| `vector` | `number[]` | embedding vector |
| `dimensions` | `integer` | vector dimension count |
| `usage` | `table?` | optional token-usage table from the host |
| `usage.input_tokens` | `integer?` | optional input token count |
| `usage.output_tokens` | `integer?` | optional output token count |

`llm(...)` success envelope:

```lua
{
    ok = true,
    assistant = "...",
    usage = {
        input_tokens = 123,
        output_tokens = 456,
    },
}
```

Field notes:

| Field | Type | Meaning |
| --- | --- | --- |
| `ok` | `boolean` | `true` on success |
| `assistant` | `string` | model response text |
| `usage` | `table?` | optional token-usage table |

Failure envelope:

```lua
{
    ok = false,
    error = {
        code = "provider_error",
        message = "model provider failed",
        provider_message = "raw provider error after host redaction",
        provider_code = "model_not_found",
        provider_status = 400,
    },
}
```

Error fields:

| Field | Type | Meaning |
| --- | --- | --- |
| `error.code` | `string` | stable error code |
| `error.message` | `string` | skill-facing error summary |
| `error.provider_message` | `string?` | provider message that the host chose to expose |
| `error.provider_code` | `string?` | provider-native error code |
| `error.provider_status` | `integer?` | provider status such as an HTTP status |

Current stable error codes:

- `model_unavailable`
- `invalid_argument`
- `provider_error`
- `timeout`
- `budget_exceeded`
- `internal_error`

### 4.5.3 Behavior Rules

- `status()` always exists and derives capability state from registered callbacks.
- `has()` only recognizes `embed` and `llm`. Unknown capabilities, wrong argument counts, or wrong types all return `false` instead of raising.
- `embed()` accepts exactly one non-empty string and does not support batch input.
- `llm()` accepts exactly two non-empty strings and does not support messages, tools, streaming, or thinking controls.
- `embed()` and `llm()` return failure envelopes for invalid arguments, missing capability registration, and provider failures instead of throwing Lua runtime errors.
- Lua cannot pass `model`, `temperature`, `max_tokens`, `base_url`, `api_key`, `dimensions`, or provider-specific parameters.
- LuaSkills passes caller context to host callbacks for audit and cost attribution, but does not expose that context through the model API.
- Model configuration, API keys, provider routing, timeouts, budgets, and redaction remain host responsibilities.

Host integration references:

- [Runtime architecture model capability boundary](architecture/runtime-model.md#standard-model-capability-boundary)
- [FFI and SDK model capability quick path](ffi/overview.md#model-capability-quick-path)
- [Chinese FFI model callback guide](zh-CN/ffi/integration-guide.md#98-模型能力-callback)

## 4.6 `vulcan.host.*`

`vulcan.host.*` is a fixed bridge for host-registered tools.
It is intentionally narrower than arbitrary `vulcan.xxx` injection: Lua can list, probe, and call host tools, but it cannot create new top-level namespaces or register host tools itself.

### 4.6.1 API Reference

| API | Parameters And Types | Return Value | Notes |
| --- | --- | --- | --- |
| `vulcan.host.list()` | none | `table` | host-visible tool metadata; exact shape is host-defined |
| `vulcan.host.has(tool_name)` | `tool_name: string` | `boolean` | probes one host tool |
| `vulcan.host.has_tool(tool_name)` | `tool_name: string` | `boolean` | alias of `has` |
| `vulcan.host.call(tool_name, args)` | `tool_name: string`; `args: table` | `table` | calls one host tool and returns the result |

Minimal example:

```lua
if not vulcan.host.has("vault.lookup") then
    return {
        ok = false,
        reason = "host-tool-unavailable",
    }
end

local result = vulcan.host.call("vault.lookup", {
    key = "demo-secret",
})

if not result.ok then
    return result
end

return result.value
```

### 4.6.2 Recommended Result Envelopes

Recommended success envelope:

```lua
{
    ok = true,
    value = {
        text = "resolved value",
    },
    meta = {
        elapsed_ms = 120,
    },
}
```

Recommended error envelope:

```lua
{
    ok = false,
    error = {
        code = "tool_not_found",
        message = "host tool not found: vault.lookup",
    },
}
```

### 4.6.3 Runtime Normalization Rules

- `list()` returns an empty table `{}` when the host has not registered a host-tool callback.
- `has()` / `has_tool()` return `false` when the host has not registered a callback.
- The host callback behind `has()` may return:
  - one `boolean`
  - one object with boolean `exists`, `has`, or `available`
- `call()` requires `args` to be a Lua table. An empty table stays an empty JSON object `{}`, not an empty array `[]`.
- If the host `call` callback returns an object, the runtime forwards it as-is.
- If the host `call` callback returns one scalar or array, the runtime wraps it as:

```lua
{
    ok = true,
    value = <host raw value>,
}
```

- When no host callback is registered, `call()` returns:

```lua
{
    ok = false,
    error = {
        code = "host_tool_callback_missing",
        message = "...",
    },
}
```

- When callback dispatch itself fails, `call()` returns:

```lua
{
    ok = false,
    error = {
        code = "host_tool_callback_error",
        message = "...",
    },
}
```

### 4.6.4 Behavior Rules

- `args` must be a Lua table. Use explicit keys for object-shaped inputs.
- Streaming is not part of this bridge. Host tools should return one complete table result.
- Permissions, timeouts, audit, and secret handling remain host responsibilities.
- Standard model capabilities should use `vulcan.models.*`, not a long-term generic host-tool contract.

## 5. `vulcan.runtime.*`

### 5.1 `vulcan.runtime.log(level, message)`

Writes one runtime log message to the host.

```lua
vulcan.runtime.log("info", "skill started")
vulcan.runtime.log("warn", "budget is low")
vulcan.runtime.log("error", "query failed")
```

API contract:

| Item | Type | Meaning |
| --- | --- | --- |
| `level` | `string` | usually `info`, `warn`, or `error`; runtime matches by text |
| `message` | `string` | text written to host logging |
| Return value | `nil` | no business value is returned |

Notes:

- If `level` contains `error` or `fatal`, the runtime logs it as an error.
- If `level` contains `warn`, the runtime logs it as a warning.
- All other values are logged as normal information.
- This capability is available in normal skill VMs.
- It is disabled in `vulcan.runtime.lua.exec(...)`.

### 5.2 `vulcan.runtime.cwd()`

Returns the current process working directory.

```lua
local cwd = vulcan.runtime.cwd()
```

| Return Value | Type | Meaning |
| --- | --- | --- |
| `cwd` | `string` | current host process working directory |

Notes:

- This is the raw runtime process working directory.
- Do not confuse it with the conventional entry argument `PWD`.
- When a skill needs to follow the host-managed project/workspace scope, prefer explicit entry input `PWD`; the two values may differ.

### 5.3 `vulcan.runtime.temp_dir`

Host-injected temporary directory path. It may be `nil`.

```lua
local temp_dir = vulcan.runtime.temp_dir
```

| Field | Type | Meaning |
| --- | --- | --- |
| `vulcan.runtime.temp_dir` | `string \| nil` | host-configured temp directory, or `nil` when absent |

### 5.4 `vulcan.runtime.resources_dir`

Host-injected resources directory path. It may be `nil`.

```lua
local resources_dir = vulcan.runtime.resources_dir
```

| Field | Type | Meaning |
| --- | --- | --- |
| `vulcan.runtime.resources_dir` | `string \| nil` | host-configured resources directory, or `nil` when absent |

### 5.5 `vulcan.runtime.overflow_type`

Currently exposes two fixed constants:

- `vulcan.runtime.overflow_type.truncate`
- `vulcan.runtime.overflow_type.page`

They are mainly used by host-side budget and overflow policy logic.

### 5.6 `vulcan.runtime.internal`

Currently exposes these fields:

- `tool_name`
- `skill_name`
- `entry_name`
- `root_name`
- `luaexec_active`
- `luaexec_caller_tool_name`

Field notes:

| Field | Type | Meaning |
| --- | --- | --- |
| `tool_name` | `string \| nil` | current externally visible canonical tool name |
| `skill_name` | `string \| nil` | current skill identifier |
| `entry_name` | `string \| nil` | current local entry name inside the skill |
| `root_name` | `string \| nil` | current runtime root layer such as `ROOT`, `PROJECT`, or `USER` |
| `luaexec_active` | `boolean` | whether the current call is inside the `vulcan.runtime.lua.exec(...)` path |
| `luaexec_caller_tool_name` | `string \| nil` | outer tool name that triggered the current `luaexec` call |

These are **internal execution-context fields**. Use them for debugging, logging, and diagnostics only, not as long-term public protocol.

### 5.7 `vulcan.runtime.lua.exec(input)`

Runs one isolated inline Lua runtime call and returns a **Markdown string**, not a normal Lua table.

Supported input fields:

| Field | Type | Required | Meaning |
| --- | --- | --- | --- |
| `task` | `string` | no | human-readable task summary shown in the rendered result |
| `code` | `string` | one of `code` / `file` | inline Lua source to execute |
| `file` | `string` | one of `code` / `file` | Lua file path to execute |
| `args` | `table` | no | structured argument object injected into the execution context; defaults to an empty object |
| `timeout_ms` | `integer` | no | timeout in milliseconds; defaults to `60000` |

Minimal example:

```lua
local rendered = vulcan.runtime.lua.exec({
    task = "inspect args",
    code = [[
        print("hello", args.name)
        return { ok = true, name = args.name }
    ]],
    args = {
        name = "codex",
    },
})

return rendered
```

Return and behavior:

| Item | Type | Meaning |
| --- | --- | --- |
| Return value | `string` | rendered Markdown text |
| `print(...)` output | text | collected and appended into the rendered result |
| code return value | any JSON-serializable value | rendered into the result text instead of being returned as a Lua table |

Important limits:

- The return value is rendered Markdown text, not a structured Lua table.
- The isolated environment overrides global `print` and collects output into the result text.
- The isolated environment disables:
  - `vulcan.runtime.log`
  - `vulcan.cache.put`
  - `vulcan.cache.get`
  - `vulcan.cache.delete`
  - `vulcan.runtime.lua.exec` itself, preventing recursive luaexec calls
- This environment uses an internal synthetic request context. Inside `luaexec`, values such as:
  - `vulcan.context.client_info.name`
  - `vulcan.context.request.transport_name`
  usually use internal identifiers such as `luaexec_call`, not the real external client.
- The isolated execution path has its own VM pool:
  - Default `min_size=1 / max_size=4 / idle_ttl_secs=60`.
  - Hosts can override it through `LuaRuntimeHostOptions.runlua_pool_config`.
  - This only affects `vulcan.runtime.lua.exec(...)` and does not change the normal skill VM pool.
  - The runtime no longer supports configuring a separate external executor path for `vulcan.runtime.lua.exec(...)`.
- This call resets request-scoped state and does not inherit arbitrary ad hoc globals you may have written into the outer VM.

### 5.8 `vulcan.runtime.skills.*`

These capabilities let a skill ask the host to install, update, enable, disable, or uninstall skills.

The formal layer model is:

```text
ROOT -> PROJECT -> USER
```

`ROOT` is system-controlled, and a runtime must receive a `ROOT` layer when it starts or loads. Normal skills cannot use `vulcan.runtime.skills.*` to install, update, uninstall, enable, or disable `ROOT` skills. The normal bridge only targets currently existing `PROJECT` and `USER` layers exposed by the host.

`vulcan.runtime.skills.*` is always equivalent to `DelegatedTool` authority: it cannot see `ROOT` skills and cannot write `ROOT`. FFI query and prompt-completion entry points under `DelegatedTool` also do not return `ROOT` entries, help, or ROOT tool ownership. `call_skill` and `run_lua` are runtime execution surfaces: they can call currently active skills, but they are not skill-management authorization boundaries. If `ROOT` already contains the same `skill_id`, normal-layer install and update are rejected; normal-layer uninstall can still clean stale same-name entries from `PROJECT` or `USER`.

The formal bridge should include:

- `vulcan.runtime.skills.enabled`
- `vulcan.runtime.skills.status()`
- `vulcan.runtime.skills.layers()`
- `vulcan.runtime.skills.install(input)`
- `vulcan.runtime.skills.update(input)`
- `vulcan.runtime.skills.uninstall(input)`
- `vulcan.runtime.skills.enable(input)`
- `vulcan.runtime.skills.disable(input)`

Stable fields:

| API / Field | Type | Meaning |
| --- | --- | --- |
| `vulcan.runtime.skills.enabled` | `boolean` | whether host policy enabled the management bridge |
| `status().enabled` | `boolean` | same policy state |
| `status().callback_registered` | `boolean` | whether the host actually registered a callback |
| `status().mode` | `string` | currently fixed to `host_callback` |
| `status().message` | `string` | current bridge-state description |

`layers()` returns the layer labels that the host allows the normal bridge to operate on. Recommended fields include:

- `default`
- `writable`
- `labels`
- `layers`

`labels` should include only currently existing `PROJECT` and `USER` layers; it should not include `ROOT`. Without a project context, the runtime returns only `USER`. If only `ROOT` exists, it returns an empty list and top-level `writable=false`. When the bridge is disabled, layer discovery can still work, but top-level `writable` and each layer's `writable` must be `false`. If future install, update, or uninstall inputs allow selecting a layer, they should only accept labels returned by `layers()`.

For the full layer and management boundary model, see [Skill root layer policy](zh-CN/architecture/skill-root-layer-policy.md).

Notes:

- These capabilities only execute when the host explicitly enables `enable_skill_management_bridge`.
- Even when host policy enables the bridge, missing callbacks return explicit errors.
- `install/update/uninstall/enable/disable` request and response payloads are part of the host bridge contract. The runtime dispatches them but does not define your business schema.
- If one payload explicitly targets the `ROOT` layer, the runtime rejects it before dispatch.

### 5.9 Managed Python And Node Child Runtimes

Lua remains the first-class orchestration layer. Managed Python and Node.js runtimes are child execution surfaces that Lua calls through `vulcan.runtime.python.*` and `vulcan.runtime.node.*`; they do not replace the skill entry model, the Lua VM pool, or the host SDK contract.

Both ordinary Skill calls and dedicated System Plugin leases carry one trusted package context. A System lease is created with a strict `system_package = { id, root, dependencies_file }` descriptor; `root` must be an absolute strict descendant of the engine's `system_lua_lib` directory, cannot contain Lua search-path metacharacters, and is revalidated before every eval. `dependencies_file` is resolved as a package-relative regular file without allowing path or symlink escape. Optional `workspace_root` must be an existing absolute directory, and `cwd` must resolve under either that workspace or the package root. The package root is the only System Plugin Lua module root, so sibling plugins do not share `require(...)` scope or managed dependency identity.

Inside a System Plugin lease, the runtime also exposes:

| Field | Type | Meaning |
| --- | --- | --- |
| `vulcan.runtime.system_plugin` | recursively read-only userdata view | `id`, canonical `root`, `lease_id`, `sid`, and `generation` for the bound package lease |
| `vulcan.runtime.workspace_root` | `string \| nil` | canonical workspace root explicitly authorized by the host |
| `vulcan.runtime.mounts` | recursively read-only userdata view | host-owned structured mount metadata |

The two userdata views support indexing, `pairs` / `ipairs`, length, and normal JSON-result serialization. The three host-owned root fields cannot be assigned or replaced. Dedicated System VMs remove global `rawset` and the Lua `debug` library, preventing metatable bypass. These fields replace the old assumption that every System runtime context is absent or `nil`; entry-specific `vulcan.context.skill_dir / entry_dir / entry_file` fields remain separate and may still be `nil` because a lease eval is not a Skill entry call.

The normal flow is:

1. The skill declares Python or Node.js needs in `dependencies.yaml`.
2. The runtime root is prepared once with the managed runtime fetch script.
3. Lua calls a Python or Node.js file through `vulcan.runtime.python.invoke(...)` or `vulcan.runtime.node.invoke(...)`.
4. The runtime lazily creates a versioned environment, keeps a pooled worker alive, and returns a structured result to Lua.

Prepare runtimes before invoking skills that declare managed child runtimes:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/deps/fetch_managed_runtimes.ps1 -RuntimeRoot <runtime_root> -Target all
```

```bash
RUNTIME_ROOT=<runtime_root> scripts/deps/fetch_managed_runtimes.sh all
```

The fetch scripts place portable runtimes under `<runtime_root>/dependencies/runtimes/...`. Runtime-created package environments live under `<runtime_root>/dependencies/envs/...` and are keyed by runtime version, platform, package manager version, and lockfile content.

Minimal `dependencies.yaml`:

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

`node_runtime.version` must be one exact semantic version, not a range, tag, or partial version. The managed asynchronous ESM worker protocol requires Node.js `22.0.0` or newer; invalid SemVer and older versions are rejected before environment resolution or worker launch.

Lua invocation:

```lua
local python_result = vulcan.runtime.python.invoke({
    file = "python/handler.py",
    handler = "main",
    timeout_ms = 30000,
    args = {
        text = "hello",
    },
})

if not python_result.ok then
    error("python failed: " .. tostring(python_result.error))
end

local node_result = vulcan.runtime.node.invoke({
    file = "node/handler.mjs",
    handler = "main",
    timeout_ms = 30000,
    args = python_result.value,
})

if not node_result.ok then
    error("node failed: " .. tostring(node_result.error))
end
```

Python handlers receive JSON-compatible arguments and context:

```python
def main(args, ctx):
    return {
        "text": args.get("text", ""),
        "ctx_is_dict": isinstance(ctx, dict),
    }
```

Node.js handlers are ESM modules. The default handler is `default`, with `main` as a fallback when `handler` is omitted; explicit `handler = "main"` calls the named export:

```javascript
export async function main(args, ctx) {
  return {
    text: args.text || "",
    ctxIsObject: ctx && typeof ctx === "object",
  };
}
```

Status APIs:

| API | Return Value | Notes |
| --- | --- | --- |
| `vulcan.runtime.python.status()` | table | status for the current skill's `python_runtime` declaration |
| `vulcan.runtime.node.status()` | table | status for the current skill's `node_runtime` declaration |

Stable status fields:

| Field | Type | Meaning |
| --- | --- | --- |
| `available` | `boolean` | whether the declaration can be resolved to a prepared runtime |
| `configured` | `boolean` | whether the skill declared the runtime in `dependencies.yaml` |
| `ready` | `boolean` | whether the package environment has already been created |
| `runtime` | `"python" \| "node"` | child runtime kind |
| `runtime_version` | `string?` | declared runtime version when resolved |
| `runtime_executable` | `string?` | resolved child runtime executable path |
| `package_manager` | `string?` | `uv` or `pnpm` |
| `package_manager_version` | `string?` | declared package manager version |
| `package_manager_executable` | `string?` | resolved package manager executable path |
| `env_hash` | `string?` | stable package environment hash |
| `env_dir` | `string?` | resolved package environment directory |
| `message` | `string?` | human-readable state description |
| `error` | `string?` | resolution error when available/configured cannot become ready |

Invoke input:

| Field | Type | Required | Notes |
| --- | --- | --- | --- |
| `file` | `string` | yes | safe relative path inside the current skill directory |
| `handler` | `string` | no | Python defaults to `main`; Node defaults to `default` with `main` fallback |
| `args` | JSON-compatible Lua value | no | passed to the child handler |
| `timeout_ms` | positive integer | no | per-call timeout; overrides the engine `managed_runtime_config.invoke_default_timeout_ms`; both omitted means unlimited; timed-out workers are discarded |

Invoke result:

| Field | Type | Meaning |
| --- | --- | --- |
| `ok` | `boolean` | whether the child handler completed successfully |
| `value` | JSON-compatible value | handler return value, or `nil`/`null` on failure |
| `stdout` | `string` | captured Python stdout or Node `console.log/info` output |
| `stderr` | `string` | captured Python stderr or Node `console.warn/error` output |
| `error` | `string?` | child error message |
| `trace` | `string?` | child traceback or stack trace when available |
| `status` | `string?` | worker-level failure status such as timeout or broken output |
| `timed_out` | `boolean?` | true when the call exceeded `timeout_ms` |
| `worker_reused` | `boolean` | whether this call used an existing pooled worker |
| `env_hash` | `string` | package environment hash used by the call |
| `env_dir` | `string` | package environment directory used by the call |

Behavior notes:

- The first invoke for a new environment hash creates the package environment. Later calls reuse the environment marker and usually reuse a live worker.
- Every live worker or persistent session snapshot retains a shared cross-process environment lifecycle lease. Final environment publication or replacement uses a nonblocking exclusive lease; while any worker or session remains active, the operation fails immediately with a stable `managed runtime environment is busy with active lifecycle leases` error instead of waiting or replacing files underneath the user.
- Python modules are loaded by file path and cached inside the Python worker. Each worker fixes its one snapshot root at the front of `sys.path`, removes the cwd-derived empty entry, and rejects any later request that attempts to change that root.
- Node.js modules are imported with native Node ESM behavior. Default, named, namespace, relative, side-effect, and dynamic imports should follow Node's normal rules when dependencies are declared in `package.json` and locked by `pnpm-lock.yaml`.
- For Node.js, the current skill directory is copied into the managed environment import root before invocation. This keeps bare dependency resolution inside the managed `node_modules` directory and avoids Windows symlink privilege requirements.
- On Unix, pooled workers enter the pinned snapshot directory before `exec`; every requested relative entry is validated component by component with descriptor-relative, no-follow `openat` calls. On Windows, workers receive absolute share-locked snapshot source paths and use the snapshot's short drive or UNC-share root as a neutral cwd, so they neither inherit the host cwd nor pass a long snapshot path as `CreateProcessW.lpCurrentDirectory`.
- Managed `status`, pooled `invoke`, and System Plugin persistent sessions are supported on Windows x86_64, Linux x86_64/aarch64, and macOS x86_64/aarch64. Windows ARM/aarch64 is the sole explicit persistent-session exception.
- Child code should return JSON-serializable values. Non-serializable results are reported as structured child errors instead of crashing the Lua skill VM.
- `stdout` and `stderr` are captured into the result so Lua can decide whether to expose, redact, or log them.
- Skills should not rely on system Python, system Node.js, external `node_modules`, or undeclared packages.

#### Persistent `session.open(...)`

Only dedicated System Plugin leases may use `vulcan.runtime.python.session.open(...)` or `vulcan.runtime.node.session.open(...)` when the child process must remain alive across multiple evaluations of the same lease VM. Persistent managed sessions support Windows x86_64, Linux x86_64/aarch64, and macOS x86_64/aarch64 with the same package snapshot, dependency, event, and process-tree cleanup rules. Windows ARM/aarch64 returns `persistent_session.supported=false` with the stable reason `windows_arm_is_not_supported` before environment creation, snapshot creation, or session reservation. Ordinary Skills retain supported `status` and `invoke` operations but cannot open persistent managed sessions. Every managed runtime `status()` response includes `persistent_session.supported`, `target_os`, `target_arch`, and an unsupported `reason` when applicable. The returned userdata uses the same `write`, `read`, `status`, `close`, and `kill` methods documented for `vulcan.process.session`, but the executable and dependency environment come only from the current trusted package declaration.

Strict open input:

| Field | Type | Required | Meaning |
| --- | --- | --- | --- |
| `file` | `string` | yes | existing package-relative Python or Node source file; traversal and symlink escape are rejected |
| `args` | dense `string[]` | no | direct child-process arguments; no shell expansion |
| `cwd` | `string` | no | package-relative or absolute directory contained by the package root or the explicitly authorized System workspace root; defaults to the package root |
| `stdout_encoding` | `string` | no | stdout decoding; defaults to the host runtime encoding |
| `stderr_encoding` | `string` | no | stderr decoding; defaults to the host runtime encoding |
| `stdin_encoding` | `string` | no | stdin encoding; defaults to the host runtime encoding |
| `buffer_limit_bytes` | positive integer | no | retained byte limit per output stream; overrides the engine `managed_runtime_config.persistent_session_default_buffer_limit_bytes_per_stream`, whose stable default is 1 MiB |

Unknown fields are rejected. Python and Node both execute from a unique immutable per-session package snapshot copied through fixed, no-follow filesystem objects. Python uses the package's managed virtual environment without inherited `PYTHONHOME`, `PYTHONPATH`, or user-site packages; Node resolves bare dependencies from the exact managed `node_modules`. Symbolic links and unsupported objects in the copied package tree are rejected, and the authorized `cwd` object is pinned during process creation. Child code receives controlled package and lease metadata through `LUASKILLS_MANAGED_CONTEXT_JSON`, not an arbitrary copy of host request state.

Minimal cross-eval System Plugin flow:

```lua
-- First eval: keep the userdata in the persistent lease VM.
managed = vulcan.runtime.python.session.open({
    file = "python/sidecar.py",
    args = { "--stdio" },
    cwd = "python",
    stdout_encoding = "utf-8",
    stderr_encoding = "utf-8",
    stdin_encoding = "utf-8",
    buffer_limit_bytes = 1024 * 1024,
})
local status = managed:status()
return { managed_session_id = status.managed_session_id }
```

```lua
-- Later eval on the same lease_id: reuse the same userdata and process.
managed:write(vulcan.json.encode(args.message) .. "\n")
local output = managed:read({ timeout_ms = 2000, max_bytes = 65536 })
return {
    managed_session_id = managed:status().managed_session_id,
    stdout = output.stdout,
    stderr = output.stderr,
    stdout_total_bytes = output.stdout_total_bytes,
    stdout_dropped_bytes = output.stdout_dropped_bytes,
}
```

Only managed Python/Node session status tables include `managed_session_id`; ordinary `vulcan.process.session` status tables do not. The id precisely correlates the Lua userdata with host-side `RuntimeManagedSessionEvent.managed_session_id`.

Lifecycle rules are deterministic:

- A failed lease eval rolls back and terminates every managed session opened by that eval, even if Lua stored the userdata before raising the error.
- Explicit lease close, same-SID replacement, lease expiration, VM destruction, and engine destruction terminate the complete child process tree and retire package-owned workers.
- Ordinary Skill-root reload does not replace the dedicated System lease manager, so an active System Plugin session survives unrelated Skill reloads until its own lease lifecycle ends.
- Dropping or collecting the userdata also terminates the process tree. Python/Node package snapshots are deleted only after process teardown.
- The snapshot retains its environment's shared lifecycle lease through process teardown and snapshot cleanup, so environment replacement cannot race a still-live session or worker.
- `session:close(...)` and `session:kill()` terminate descendants as well as the direct child.
- Background stdout, stderr, exit, and failure watchers never invoke Lua. System Plugin sessions publish bounded engine events for the host to poll or wait on; a host wake callback may only wake or schedule host work and must not synchronously enter Lua.

For the complete System lease, persistent-session, host-event, and cleanup workflow, see the [System Plugin managed runtime guide](system-plugin-managed-runtime.md). The [Managed Runtime Example](../examples/managed_runtime/README.md) is a working ordinary-Skill package for the pooled `status` / `invoke` path. During repository development, the layout checker and isolated smoke scripts provide a fast end-to-end verification path:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/debug-tools/managed_runtime_smoke.ps1
```

```bash
bash scripts/debug-tools/managed_runtime_smoke.sh
```

### 5.10 `vulcan.config.*`

These capabilities read and maintain **declared configuration for the current skill package**. Declarations belong to the top-level `config` field in the package `skill.yaml`, and every entry in that package shares the same namespace.

Currently available:

| API | Parameters And Types | Return Value | Notes |
| --- | --- | --- | --- |
| `vulcan.config.get(key)` | `key: string` | `string \| nil` | returns the stored value or declared default; key must be declared |
| `vulcan.config.has(key)` | `key: string` | `boolean` | checks for a stored value or declared default |
| `vulcan.config.set(key, value)` | `key: string`; `value: string` | `true` | validates, normalizes, and persists one declared value |
| `vulcan.config.delete(key)` | `key: string` | `boolean` | deletes the stored value and may reveal the declared default |
| `vulcan.config.list()` | none | `table<string, string>` | lists effective values for declared keys only |
| `vulcan.config.describe()` | none | `table` | returns names, types, constraints, package-authored enum metadata and descriptions, and state without values |
| `vulcan.config.status()` | none | `table` | returns completeness, missing values, invalid values, and the orphaned count |

Minimal example:

```lua
local status = vulcan.config.status()
if not status.complete then
    return [[This skill package configuration is incomplete.
Ask the AI to call the host runtime-config describe action, then use set only
after host or user authorization. If mutation is unavailable, request the missing values.]]
end

local api_token = vulcan.config.get("api_token")
return use_service(api_token)
```

Notes:

- Declarations support `integer`, `string`, `float`, `enum`, and `boolean`; persistence and Lua values use normalized strings.
- Undeclared keys are rejected by `get/has/set/delete`, and `list` never exposes orphaned records.
- Configuration is isolated by package. A nested call switches to the target package and restores the caller afterward.
- These APIs **require one active skill context**. They raise an error in system runtimes that do not currently represent one skill identity.
- Missing or invalid configuration does not prevent package loading. Check it only where an entry needs it.
- The unified main config file defaults to `<runtime_root>/config/skill_config.json`; hosts can explicitly override the path.
- Human-readable declaration fields use the single language chosen by the package author. English is recommended for broadly distributed packages but is not enforced.
- The complete declaration schema, storage formats, host authorization boundary, and FFI contract are documented in [Skill package configuration](zh-CN/architecture/skill-config-system-design.md).

## 6. `vulcan.fs.*`

### 6.1 API Reference

| API | Parameters And Types | Return Value | Notes |
| --- | --- | --- | --- |
| `vulcan.fs.list(dir)` | `dir: string` | `string[]` | returns directory entry names only, not full paths |
| `vulcan.fs.read(path)` | `path: string` | `string` | reads one whole file as UTF-8 text; non-UTF-8 content errors |
| `vulcan.fs.write(path, content)` | `path: string`; `content: string` | `true` | overwrites text; does not create parent directories |
| `vulcan.fs.read_bytes(path)` | `path: string` | `string` | returns raw bytes as Base64 text |
| `vulcan.fs.write_bytes(path, base64_text)` | `path: string`; `base64_text: string` | `true` | decodes Base64 and writes raw bytes |
| `vulcan.fs.rename(old_path, new_path)` | `old_path: string`; `new_path: string` | `true` | renames or moves one file or directory |
| `vulcan.fs.copy(src_path, dst_path, options?)` | `src_path: string`; `dst_path: string`; `options?: { overwrite?: boolean }` | `boolean` | copies one regular file or one whole directory tree; returns `false` when the destination already exists and overwrite is not allowed |
| `vulcan.fs.remove(path, options?)` | `path: string`; `options?: { recursive?: boolean }` | `boolean` | removes one file or directory; returns `false` when the target is already missing |
| `vulcan.fs.mkdir(path, options?)` | `path: string`; `options?: { recursive?: boolean }` | `boolean` | creates one directory; returns `false` when the target directory already exists |
| `vulcan.fs.stat(path)` | `path: string` | `table \| nil` | returns `nil` when missing, otherwise a metadata table |
| `vulcan.fs.exists(path)` | `path: string` | `boolean` | checks whether the path exists |
| `vulcan.fs.is_dir(path)` | `path: string` | `boolean` | checks whether the path is a directory |

### 6.2 `fs.stat(path)` Result Shape

When the target exists, `vulcan.fs.stat(path)` returns:

| Field | Type | Meaning |
| --- | --- | --- |
| `kind` | `"file" \| "dir" \| "symlink" \| "other"` | normalized target kind |
| `is_file` | `boolean` | whether the target is a regular file |
| `is_dir` | `boolean` | whether the target is a directory |
| `is_symlink` | `boolean` | whether the target is a symbolic link |
| `readonly` | `boolean` | whether the metadata is read-only |
| `size` | `integer?` | file size in bytes for regular files |
| `modified_unix_ms` | `integer?` | Unix modification timestamp in milliseconds when available |

### 6.3 Example

```lua
local entries = vulcan.fs.list(vulcan.context.entry_dir)
local exists = vulcan.fs.exists(vulcan.context.entry_file)
local content = vulcan.fs.read(vulcan.context.entry_file)
local payload_base64 = vulcan.fs.read_bytes(vulcan.context.entry_file)
local info = vulcan.fs.stat(vulcan.context.entry_file)
local created = vulcan.fs.mkdir(vulcan.path.join(vulcan.context.entry_dir, "output"), {
    recursive = true,
})
```

Byte-preserving example:

```lua
local payload = vulcan.fs.read_bytes("D:/data/image.bin")

vulcan.fs.write_bytes("D:/data/image.copy.bin", payload)
```

### 6.4 Notes

- There is currently no sandbox restriction; a skill can theoretically access any path that the host process can access.
- `fs.list(dir)` returns child names only. Use `vulcan.path.join(...)` to build full paths.
- `fs.read` and `fs.write` operate on text. `fs.read` expects valid UTF-8. Use `fs.read_bytes` for non-UTF-8 content.
- `fs.read_bytes(path)` returns Base64 text, and `fs.write_bytes(path, base64_text)` accepts Base64 text and writes the original bytes. This is the preferred byte-preserving path when non-UTF-8 content must cross the Lua/host boundary.
- Prefer `fs.rename`, `fs.remove`, and `fs.mkdir` for file lifecycle changes that need stable Windows Unicode-path behavior instead of relying on native `os.rename` or `os.remove`.
- `fs.copy(src_path, dst_path)` does not overwrite an existing destination unless `{ overwrite = true }` is explicitly provided.
- Destination-existence checks are based on the path entry itself, so dangling symbolic links still count as existing destinations.
- When the source path is a directory, `fs.copy` recursively copies the whole directory tree.
- When `overwrite = true` and the destination already exists, the runtime deletes the destination path first and then copies the new content. This means the destination tree is **replaced**, not merged.
- When the source path is a directory, destination validation resolves existing parent links before deciding whether the destination falls back inside the source tree.
- Directory-tree copy currently rejects symbolic-link entries so behavior stays predictable across platforms.
- When the source path is a directory, the destination path must not equal the source directory and must not live inside the source directory; otherwise the runtime raises an explicit error instead of recursing into itself.
- `fs.remove(path, { recursive = true })` recursively deletes directories and returns `false` when the target is already missing.
- `fs.remove(path)` without `recursive = true` attempts a normal directory removal and errors for non-empty directories.
- When the path itself is a symbolic link, `fs.remove` removes the link entry itself instead of deleting the linked target.
- `fs.mkdir(path, { recursive = true })` recursively creates directories and returns `false` when the target directory already exists.
- `fs.mkdir(path)` errors when the target path already exists but is not a directory.
- `fs.rename(old_path, new_path)` returns `true` on success and raises a runtime error for other failures.
- `fs.stat(path)` returns `nil` when the target is missing; otherwise it returns a table with fields such as `kind`, `is_file`, `is_dir`, `is_symlink`, `readonly`, optional `size` for regular files, and optional `modified_unix_ms`.
- Path arguments must be strings and pass basic path syntax validation.

## 7. `vulcan.path.*`

### 7.1 API Reference

| API | Parameters And Types | Return Value | Notes |
| --- | --- | --- | --- |
| `vulcan.path.join(...)` | one or more `string` segments | `string` | joins path segments using host rules; at least one segment is required |
| `vulcan.path.dirname(path)` | `path: string` | `string` | returns the parent directory |
| `vulcan.path.basename(path)` | `path: string` | `string` | returns the terminal file name portion |
| `vulcan.path.stem(path)` | `path: string` | `string` | returns the terminal file name without extension |
| `vulcan.path.extname(path)` | `path: string` | `string` | returns the extension including the leading dot |
| `vulcan.path.normalize(path)` | `path: string` | `string` | performs lexical normalization without touching the filesystem |
| `vulcan.path.is_abs(path)` | `path: string` | `boolean` | checks whether one path is absolute |

### 7.2 Example

```lua
local config_path = vulcan.path.join(
    vulcan.context.skill_dir,
    "runtime",
    "config.json"
)
local ext = vulcan.path.extname(config_path)
```

### 7.3 Return Rules

- Returns normal path text for the host system.
- On Windows, it does not leak `\\?\` or `\\?\UNC\` verbatim prefixes directly to Lua.
- Path-typed Lua/runtime inputs are normalized by the receiving API before lookup. This includes the reserved top-level `PWD` argument, `vulcan.fs` / `vulcan.io` paths, process `program` / `cwd` fields, Runtime Lease path fields, module search roots, and managed-runtime FFI paths.
- Only verbatim absolute drive paths and verbatim UNC paths are converted. Other Windows verbatim namespaces fail with `unsupported Windows verbatim path namespace` before lookup or process creation.
- `package.path` and `package.cpath` are built only from host-visible path spelling, so Windows canonical roots cannot stall Lua module lookup through a verbatim prefix.
- Rust-internal canonical paths and native filesystem identities remain unchanged for containment, replacement, symlink, and race checks. Removing a prefix at the Lua/API boundary does not weaken those checks.
- Arbitrary JSON string values, process arguments, and environment values are not guessed to be paths and are not rewritten.
- `path.dirname(path)` returns `.` when a relative path has no parent segment.
- `path.basename(path)`, `path.stem(path)`, and `path.extname(path)` return an empty string when the terminal component is absent.
- `path.extname(path)` includes the leading dot when an extension exists, for example `.lua`.
- `path.normalize(path)` is lexical only. It folds `.` and `..` without touching the filesystem and returns `.` when the normalized result is empty.

## 8. `vulcan.process.*`

### 8.1 API Reference

| API | Parameters And Types | Return Value | Notes |
| --- | --- | --- | --- |
| `vulcan.process.exec(spec)` | `spec: string \| table` | result table | runs one one-shot child process and waits for completion |
| `vulcan.process.launchers()` | none | `table` | returns the supported `shell` parameter values and the current default |
| `vulcan.process.which(program)` | `program: string` | `string \| nil` | searches for one executable |
| `vulcan.process.session.open(spec)` | `spec: table` | process-session handle | opens one interactive child-process session |

`vulcan.process.which(program)` searches for one executable using host lookup rules. It returns one host-visible absolute path when found and `nil` otherwise. On Windows it also consults `PATHEXT` for extensionless names.

`vulcan.process.launchers()` always returns:

| Field | Type | Meaning |
| --- | --- | --- |
| `default` | `string` | current host default shell parameter value; typically `cmd` on Windows and `sh` on Unix-like hosts |
| `shells` | `array<string>` | supported `shell` parameter values in stable order; the default value is always the first item |

Minimal example:

```lua
local launchers = vulcan.process.launchers()
local shell = launchers.default

for _, name in ipairs(launchers.shells or {}) do
    if name == "bash" then
        shell = "bash"
        break
    end
    if name == "pwsh" then
        shell = "pwsh"
        break
    end
end

local command = shell == "cmd" and "dir" or "ls"
local result = vulcan.process.exec({
    command = command,
    shell = shell,
})
```

### 8.2 `exec(spec)` Request Shape

`vulcan.process.exec(spec)` supports two input forms:

1. a string, which means shell/command mode
2. a table with explicit launch fields

Minimal string mode:

```lua
local result = vulcan.process.exec("echo hello")
```

Table fields:

| Field | Type | Required | Meaning |
| --- | --- | --- | --- |
| `command` | `string` | one of `command` / `program` | shell command text |
| `program` | `string` | one of `command` / `program` | executable path or program name |
| `args` | `array` | no | only valid in `program` mode; string arrays are recommended, but numbers and booleans are stringified |
| `cwd` | `string` | no | child working directory |
| `env` | `table<string, scalar>` | no | environment variable map; values are stringified |
| `stdin` | `string` | no | one-shot stdin text written after spawn |
| `timeout_ms` | `integer` | no | positive timeout in milliseconds; omitted means no timeout |
| `shell` | `boolean \| string` | no | optional in `command` mode; booleans keep legacy compatibility, and strings must come from `vulcan.process.launchers().shells` |
| `encoding` | `string` | no | default encoding for stdout, stderr, and stdin |
| `stdout_encoding` | `string` | no | overrides stdout decoding |
| `stderr_encoding` | `string` | no | overrides stderr decoding |
| `stdin_encoding` | `string` | no | overrides stdin encoding |

`shell` rules:

- omit `shell`: `command` mode automatically uses `vulcan.process.launchers().default`
- `shell = true`: legacy-compatible shorthand for the default shell
- `shell = false`: only valid with `program` mode; `command` mode rejects it
- `shell = "cmd" | "pwsh" | "powershell" | "bash" | "zsh" | "sh"`: explicitly selects one command carrier and must appear in `vulcan.process.launchers().shells`
- `program` mode rejects shell-name strings because it does not use a shell carrier

`command` mode example:

```lua
local result = vulcan.process.exec({
    command = "echo hello",
    timeout_ms = 3000,
})
```

Example using `launchers()` to choose one shell:

```lua
local launchers = vulcan.process.launchers()
local command = launchers.default == "cmd" and "echo hello" or "printf hello"

local result = vulcan.process.exec({
    command = command,
    shell = launchers.default,
    encoding = "utf-8",
})
```

`program` mode example:

```lua
local result = vulcan.process.exec({
    program = "git",
    args = { "status", "--short" },
    cwd = vulcan.runtime.cwd(),
    env = {
        DEMO_MODE = "1",
    },
    timeout_ms = 5000,
    encoding = "utf-8",
})
```

### 8.3 `exec(spec)` Result Shape

The result table always contains:

| Field | Type | Meaning |
| --- | --- | --- |
| `ok` | `boolean` | whether the execution chain completed successfully |
| `success` | `boolean` | whether the process exited successfully without timing out |
| `code` | `integer \| nil` | exit code when available |
| `stdout` | `string` | decoded stdout text, or Base64 text when `base64` encoding is used |
| `stderr` | `string` | decoded stderr text |
| `stdout_encoding` | `string` | actual stdout decoding label |
| `stderr_encoding` | `string` | actual stderr decoding label |
| `stdout_lossy` | `boolean` | whether stdout decoding used replacement or fallback behavior |
| `stderr_lossy` | `boolean` | whether stderr decoding used replacement or fallback behavior |
| `stdout_base64` | `string \| nil` | raw stdout bytes encoded as Base64 when available |
| `stderr_base64` | `string \| nil` | raw stderr bytes encoded as Base64 when available |
| `timed_out` | `boolean` | whether the process timed out |
| `error` | `string \| nil` | human-readable execution failure summary |

### 8.4 `session.open(spec)` And Session Handle

`vulcan.process.session.open(spec)` accepts only a table:

| Field | Type | Required | Meaning |
| --- | --- | --- | --- |
| `program` | `string` | yes | executable path or program name |
| `args` | `string[]` | no | argument array; defaults to an empty array |
| `cwd` | `string` | no | child working directory |
| `encoding` | `string` | no | default encoding for stdout, stderr, and stdin |
| `stdout_encoding` | `string` | no | overrides stdout decoding |
| `stderr_encoding` | `string` | no | overrides stderr decoding |
| `stdin_encoding` | `string` | no | overrides stdin encoding |
| `buffer_limit_bytes` | `integer` | no | internal stdout/stderr buffer limit |

Session-handle methods:

| Method | Parameters And Types | Return Value | Meaning |
| --- | --- | --- | --- |
| `session:write(...)` | one or more Lua scalar values | `true` | encodes values with `stdin_encoding`, writes them to stdin, and flushes immediately |
| `session:read(options?)` | `options?: { timeout_ms?: integer, max_bytes?: integer, until_text?: string }` | result table | reads and **drains** currently captured output |
| `session:status()` | none | status table | inspects status without terminating the process |
| `session:close(options?)` | `options?: { timeout_ms?: integer }` | status table | closes stdin, waits for exit, and kills the process tree on timeout |
| `session:kill()` | none | `true` | terminates the process tree immediately |

`session:read(...)` returns:

| Field | Type | Meaning |
| --- | --- | --- |
| `stdout` | `string` | stdout drained by this read call |
| `stderr` | `string` | stderr drained by this read call |
| `stdout_encoding` | `string` | actual stdout decoding label |
| `stderr_encoding` | `string` | actual stderr decoding label |
| `stdout_lossy` | `boolean` | whether stdout decoding was lossy |
| `stderr_lossy` | `boolean` | whether stderr decoding was lossy |
| `stdout_base64` | `string \| nil` | raw stdout bytes when available |
| `stderr_base64` | `string \| nil` | raw stderr bytes when available |
| `timed_out` | `boolean` | whether this read wait timed out |

`session:status()` and `session:close(...)` return:

| Field | Type | Meaning |
| --- | --- | --- |
| `running` | `boolean` | whether the process is still running |
| `exited` | `boolean` | whether the process has exited |
| `success` | `boolean \| nil` | exit success when exited; usually `nil` while still running |
| `code` | `integer \| nil` | exit code when available |

Interactive example:

```lua
local session = vulcan.process.session.open({
    program = "python",
    args = { "-i" },
    encoding = "utf-8",
})

session:write("print(1 + 1)\n")
local output = session:read({
    timeout_ms = 1000,
    until_text = "2",
})
local status = session:close({
    timeout_ms = 3000,
})

return vulcan.json.encode({
    stdout = output.stdout,
    exited = status.exited,
    success = status.success,
})
```

### 8.5 Notes

- `exec(spec)` and `session.open(spec)` do not perform shell escaping for you. If you build command text manually, you own that safety boundary.
- `session:read(...)` drains buffered output. The next read only sees newly produced bytes.
- `session:close(...)` kills the whole process tree on timeout, not just the direct child.
- `which(program)` supports both explicit paths and `PATH` lookup. On Windows it also expands common executable extensions via `PATHEXT`.

## 9. `vulcan.os.*`

Currently provides:

- `vulcan.os.info()`

Example:

```lua
local info = vulcan.os.info()
-- info.os
-- info.arch
```

Result shape:

| Field | Type | Meaning |
| --- | --- | --- |
| `os` | `string` | normalized platform name such as `windows`, `linux`, or `macos` |
| `arch` | `string` | normalized architecture such as `x86_64`, `i686`, `aarch64`, or `armv7l` |

`vulcan.os.*` intentionally stays minimal. It provides platform information only and does not try to wrap historically problematic native `os.rename` or `os.remove` behavior.

## 10. `vulcan.json.*`

Currently provides:

- `vulcan.json.encode(value)`
- `vulcan.json.decode(text)`
- `vulcan.json.object()` / `vulcan.json.object(table)`
- `vulcan.json.array()` / `vulcan.json.array(table)`
- `vulcan.json.null`

Example:

```lua
local payload = vulcan.json.object({
    environment = vulcan.json.object(),
    binary_path_requests = vulcan.json.array(),
    contributions = vulcan.json.array({
        "shell-hook",
    }),
    optional = vulcan.json.null,
})

local text = vulcan.json.encode(payload)
local decoded = vulcan.json.decode(text)
```

Notes:

| API | Parameters And Types | Return Value | Notes |
| --- | --- | --- | --- |
| `vulcan.json.encode(value)` | any JSON-serializable Lua value | `string` | encodes JSON text |
| `vulcan.json.decode(text)` | `text: string` | typed Lua value | recursively preserves JSON Object, Array, and Null |
| `vulcan.json.object(table?)` | optional `table` | marked `table` | creates an Object-marked shallow copy; no argument creates an empty Object |
| `vulcan.json.array(table?)` | optional `table` | marked `table` | creates an Array-marked shallow copy; no argument creates an empty Array |
| `vulcan.json.null` | none | JSON Null sentinel | use inside objects or arrays when `nil` would delete a field or create a hole |

Container and Null rules:

- Every Object and Array produced by `decode` is recursively marked with its original JSON container kind. Re-encoding therefore preserves nested empty `{}` and `[]` values.
- JSON Null decodes to the same sentinel exposed as `vulcan.json.null`. It survives object fields and array elements.
- `object(table)` and `array(table)` shallow-copy the source table and do not replace its metatable or identity. Nested values retain their existing identities.
- Container type markers use protected shared metatables. Ordinary Lua code cannot replace or mutate the runtime marker through `getmetatable`.
- An Object allows only UTF-8 string keys.
- An Array allows only dense, one-based positive integer keys. String keys, mixed keys, non-positive indexes, and holes raise an explicit encoding error.
- Constructors accept either no argument or exactly one table. Other argument shapes raise an explicit error.

Scalar encoding rules:

- `nil` -> JSON Null when passed as the encoded value
- `vulcan.json.null` -> JSON Null
- `boolean` -> JSON boolean
- `integer` / `number` -> JSON number
- `string` -> JSON string
- marked `table` -> its declared JSON Object or Array kind

Compatibility rules for unmarked Lua tables:

- when one unmarked table has `raw_len() > 0`, the runtime infers an Array and applies the same strict dense-index validation
- when `raw_len() == 0` and the table has string keys, the runtime infers an Object and requires every key to be a string
- an unmarked empty `{}` keeps the legacy behavior and encodes as `[]`; use `vulcan.json.object()` for an explicit empty Object

Unsupported input values:

- `function`
- `thread`
- `userdata`, except the runtime's documented read-only JSON context views
- `lightuserdata` other than `vulcan.json.null`

Passing those values to `vulcan.json.encode(...)` raises a runtime error.

## 11. `vulcan.cache.*`

Currently provides:

| API | Parameters And Types | Return Value | Notes |
| --- | --- | --- | --- |
| `vulcan.cache.put(value, ttl_sec?)` | `value: JSON-serializable Lua value`; `ttl_sec?: integer` | `string` | stores one cache value and returns the `cache_id` |
| `vulcan.cache.get(cache_id)` | `cache_id: string` | Lua value or `nil` | reads one cached value inside the current scope |
| `vulcan.cache.delete(cache_id)` | `cache_id: string` | `boolean` | deletes one cache item |

Example:

```lua
local cache_id = vulcan.cache.put({
    summary = "warm result",
}, 60)

local cached = vulcan.cache.get(cache_id)
local deleted = vulcan.cache.delete(cache_id)
```

Notes:

- The serialization rules for `value` are the same as `vulcan.json.encode(...)`.
- Cache scope first falls back to the current `tool_name`, then the current `skill_name`.
- If neither is available, it falls back to the internal `__runtime` scope.
- Inside `vulcan.runtime.lua.exec(...)`, cache APIs are actively removed and unavailable.
- `cache_id` is a scope-local identifier. Do not assume it is stable across skills or host instances.

## 12. `vulcan.context.*`

`vulcan.context` reads the current request and current-entry runtime context.

### 12.1 Top-Level Field Overview

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `request` | `table` | empty table | raw request context |
| `client_info` | `table \| nil` | `nil` | current client metadata |
| `client_capabilities` | `table` | empty table | host-injected client capability snapshot |
| `client_budget` | `table` | empty table | host-injected budget snapshot |
| `tool_config` | `table` | empty table | host-injected tool configuration |
| `host_result` | `table` | empty table | host structured-result capability view |
| `skill_dir` | `string \| nil` | `nil` | current skill root directory |
| `entry_dir` | `string \| nil` | `nil` | current entry directory |
| `entry_file` | `string \| nil` | `nil` | current entry absolute file path |

### 12.2 `vulcan.context.request`

The original host-provided request context object. Defaults to an empty object.

Common fields come from:

- `transport_name`
- `session_id`
- `request_id`
- `client_name`
- `client_info`
- `client_capabilities`

This structure is host-defined. Skills should not assume one exact field is always present.

### 12.3 `vulcan.context.client_info`

Current request client metadata. Common fields:

- `kind`
- `name`
- `version`

Notes:

- If the host does not inject `client_info`, this may be `nil`.
- If you see `name = "luaexec_call"` inside `luaexec`, that is the synthetic context for internal isolated execution, not the real external client.

### 12.4 `vulcan.context.client_capabilities`

Host-provided client capability object. Defaults to an empty object.

### 12.5 `vulcan.context.client_budget`

Host-parsed budget snapshot object. Defaults to an empty object.

The host decides the shape, but common fields include:

- `client_name`
- `tool_name`
- `skill_name`
- `tool_result`
- `file_read`

### 12.6 `vulcan.context.tool_config`

Host-parsed tool config object. Defaults to an empty object.

### 12.7 `vulcan.context.skill_dir / entry_dir / entry_file`

File context for the currently executing skill:

- `skill_dir`: current skill directory.
- `entry_dir`: current entry script directory.
- `entry_file`: current entry script full path.

Notes:

- In normal skill calls, all three are usually available.
- In some runlua, help, or non-skill-file scenarios, they may be `nil`.
- In a System Plugin lease eval, these three **entry-specific** fields remain `nil` because the eval is not a Skill entry call. This does not mean the package context is absent: use the recursively read-only `vulcan.runtime.system_plugin` view for package identity and `vulcan.runtime.workspace_root / mounts` for host-authorized context.
- The current implementation converts Windows verbatim drive/UNC paths to their ordinary equivalents so Lua receives normal system paths; unsupported verbatim namespaces are rejected.

### 12.8 `vulcan.context.host_result`

The standardized host structured-result bridge view.

Current recommended fields:

- `enabled`
- `allowed_kinds`
- `max_payload_bytes`

Notes:

- In the current runtime baseline, the top-level `host_result` table always exists, but it typically does not contain `enabled = true` unless the host explicitly turned it on.
- Skills that support structured host results should prefer this normalized view over reading raw `client_capabilities.host_result` fields directly.
- The current recommended canonical result kind is `change_set`, used to return IDE-grade operation results back to the host.

### 12.9 Structured fourth return value

When the host explicitly enables `host_result`, one skill may return:

```lua
return content, overflow_mode, template_hint, host_result
```

Where:

- `content` remains the main text result.
- `overflow_mode` and `template_hint` keep their existing text-path meaning.
- the fourth return value `host_result` is a separate host-structured result source and does not replace the main text result.

Recommended shape:

```lua
return "Applied 1 edit.", nil, nil, {
    kind = "change_set",
    payload = {
        mode = "applied",
        summary = "Updated one file.",
        files = {
            {
                change = "modify",
                path = "D:/projects/demo/src/example.lua",
                hunks = {
                    {
                        before = "local a = 1\nlocal b = 2",
                        delete = {
                            { line = 10, content = "local x = 1" },
                            { line = 11, content = "return x" },
                        },
                        insert = {
                            { line = 10, content = "local x = 2" },
                            { line = 11, content = "local y = 3" },
                            { line = 12, content = "return x + y" },
                        },
                        after = "end\nreturn M",
                    },
                },
            },
        },
    },
}
```

Notes:

- When the host does not enable `host_result`, the fourth return value is ignored.
- `host_result` should stay JSON-serializable.
- For skill authors, `change_set` exists to provide operation-level results, not to replace `git diff`.
- `change_set.payload.files` should now always be present and should use absolute paths.
- When `change = "modify"`, one file record must provide one non-empty `hunks` array; every hunk must include `before`, `after`, `delete`, and `insert`.
- `before` and `after` should be contiguous context strings immediately adjacent to the changed block, not whole-file snapshots.
- `delete[].line` uses old-file line numbers, while `insert[].line` uses new-file line numbers after insertion, and both lists should stay sorted in ascending order.
- When `change = "create"`, the file record should directly provide full-file `content`.
- When `change = "delete"`, two content modes are supported:
  - `content_mode = "full"`, or omit `content_mode` entirely. In this mode the record should provide full-file `content`; `total_line_count` is optional on input, and the runtime will fill both `content_mode = "full"` and `total_line_count` automatically.
  - `content_mode = "truncated"`. In this mode the record must provide `total_line_count`, `content_head`, and `content_tail`; `content_head` is the leading snippet, `content_tail` is the trailing snippet, and the middle section is always considered omitted.
- Strong guidance for skill authors: handle oversized delete results proactively inside the skill whenever possible, and decide before returning whether one truncated snippet form is more appropriate; runtime auto-truncation is only a fallback safety net and should not be treated as the primary path.
- The runtime normalization rules for `change = "delete"` are stable:
  - If the skill returns full `content`, the runtime first computes `total_line_count`.
  - When the deleted content stays within `500` lines, the runtime keeps the full body and normalizes the record to `content_mode = "full"`.
  - When the deleted content exceeds `500` lines, the runtime forcibly rewrites the record to `content_mode = "truncated"`, even if the skill returned the full `content`.
  - The forced truncation shape is fixed: the first `50` lines go to `content_head`, the last `50` lines go to `content_tail`, and the middle section is not returned.
- When `change = "rename"`, the file record should provide both `old_path` and `new_path`, and both must be absolute paths.

Delete record example:

```lua
{
    change = "delete",
    path = "D:/projects/demo/src/legacy.lua",
    content = "line 1\nline 2\nline 3\n"
}
```

The legacy form above remains compatible, but the runtime normalizes the host-facing payload into:

```json
{
  "change": "delete",
  "path": "D:/projects/demo/src/legacy.lua",
  "content_mode": "full",
  "total_line_count": 3,
  "content": "line 1\nline 2\nline 3\n"
}
```

When the deleted file exceeds `500` lines, the runtime automatically rewrites it into:

```json
{
  "change": "delete",
  "path": "D:/projects/demo/src/legacy.lua",
  "content_mode": "truncated",
  "total_line_count": 1200,
  "content_head": "Joined text of the first 50 lines",
  "content_tail": "Joined text of the last 50 lines"
}
```

## 13. `vulcan.deps.*`

Current fields:

| Field | Type | Meaning |
| --- | --- | --- |
| `vulcan.deps.tools_path` | `string \| nil` | current skill tools dependency directory |
| `vulcan.deps.lua_path` | `string \| nil` | current skill Lua dependency directory |
| `vulcan.deps.ffi_path` | `string \| nil` | current skill FFI dependency directory |

These are dependency root paths for the current skill:

- Tool dependency directory.
- Lua dependency directory.
- FFI dependency directory.

Example:

```lua
local lua_lib_root = vulcan.deps.lua_path
local ffi_root = vulcan.deps.ffi_path
```

Notes:

- These paths depend on the current skill root and host dependency layout.
- If there is no valid current skill context, they are `nil`.
- In a System Plugin lease eval, these legacy Skill dependency-root fields remain `nil`. Managed Python/Node status, invoke, and session APIs instead resolve the exact `dependencies_file` bound by `system_package`; do not use `vulcan.deps.*` to infer whether a System package has declared dependencies.
- Skills should rely on these protocol-exposed paths and should not guess the host's physical directory layout.

## 14. `vulcan.sqlite.*`

`vulcan.sqlite` is a SQLite binding isolated to the **current skill scope**.

### 14.1 Current Fields And Methods

- `vulcan.sqlite.enabled`
- `vulcan.sqlite.info()`
- `vulcan.sqlite.status()`
- `vulcan.sqlite.tokenize_text(input)`
- `vulcan.sqlite.execute_script(input)`
- `vulcan.sqlite.execute_batch(input)`
- `vulcan.sqlite.query_json(input)`
- `vulcan.sqlite.query_stream(input)`
- `vulcan.sqlite.query_stream_wait_metrics(input)`
- `vulcan.sqlite.query_stream_chunk(input)`
- `vulcan.sqlite.query_stream_close(input)`
- `vulcan.sqlite.upsert_custom_word(input)`
- `vulcan.sqlite.remove_custom_word(input)`
- `vulcan.sqlite.list_custom_words()`
- `vulcan.sqlite.ensure_fts_index(input)`
- `vulcan.sqlite.rebuild_fts_index(input)`
- `vulcan.sqlite.upsert_fts_document(input)`
- `vulcan.sqlite.delete_fts_document(input)`
- `vulcan.sqlite.search_fts(input)`

Aside from `enabled`, these methods fall into two groups:

| Category | API | Input | Return Value | Meaning |
| --- | --- | --- | --- | --- |
| Stable probe | `enabled` | none | `boolean` | whether the current skill has a SQLite binding |
| Stable probe | `info()` | none | `table` | binding information suitable for availability probing |
| Stable probe | `status()` | none | `table` | current status suitable for availability probing |
| Provider forward | every other method | one `input: table` or none | `table`, array, or scalar depending on the provider | LuaSkills only forwards the action plus current binding context |

Action mapping:

| Lua API | provider action |
| --- | --- |
| `execute_script(input)` | `execute_script` |
| `execute_batch(input)` | `execute_batch` |
| `query_json(input)` | `query_json` |
| `query_stream(input)` | `query_stream` |
| `query_stream_wait_metrics(input)` | `query_stream_wait_metrics` |
| `query_stream_chunk(input)` | `query_stream_chunk` |
| `query_stream_close(input)` | `query_stream_close` |
| `tokenize_text(input)` | `tokenize_text` |
| `upsert_custom_word(input)` | `upsert_custom_word` |
| `remove_custom_word(input)` | `remove_custom_word` |
| `list_custom_words()` | `list_custom_words` |
| `ensure_fts_index(input)` | `ensure_fts_index` |
| `rebuild_fts_index(input)` | `rebuild_fts_index` |
| `upsert_fts_document(input)` | `upsert_fts_document` |
| `delete_fts_document(input)` | `delete_fts_document` |
| `search_fts(input)` | `search_fts` |

### 14.2 Behavior Rules

- `enabled = true` means the current skill has a SQLite binding.
- `info()` and `status()` always exist.
- When SQLite is not enabled:
  - `enabled = false`
  - `info()` and `status()` return disabled-state descriptions
  - other methods error directly with `current skill has not enabled sqlite`

Stable development constraints:

- Pass one Lua table for `input`. Do not pass bare strings or bare arrays.
- Exact result shapes are defined by the active SQLite provider. LuaSkills does not normalize business fields on top.
- The host/provider also receives stable binding context such as `space_label`, `skill_id`, `binding_tag`, `database_kind`, and `default_database_path`.

### 14.3 Development Guidance

- Treat `info()` and `status()` as probing entry points.
- Check `enabled` before business calls so "capability not bound" is not mistaken for a query failure.
- For exact input and output fields, combine the host SQLite provider contract with:
  - [Host database provider guide](zh-CN/providers/host-database-provider-guide.md)
- If you are a skill author rather than a host implementer, prefer existing memory-skill examples over guessing provider-specific result fields.

## 15. `vulcan.lancedb.*`

`vulcan.lancedb` is a LanceDB binding isolated to the **current skill scope**.

### 15.1 Current Fields And Methods

- `vulcan.lancedb.enabled`
- `vulcan.lancedb.info()`
- `vulcan.lancedb.status()`
- `vulcan.lancedb.create_table(input)`
- `vulcan.lancedb.vector_upsert(input)`
- `vulcan.lancedb.vector_search(input)`
- `vulcan.lancedb.delete(input)`
- `vulcan.lancedb.drop_table(input)`

| Category | API | Input | Return Value | Meaning |
| --- | --- | --- | --- | --- |
| Stable probe | `enabled` | none | `boolean` | whether the current skill has a LanceDB binding |
| Stable probe | `info()` | none | `table` | binding information suitable for availability probing |
| Stable probe | `status()` | none | `table` | current status suitable for availability probing |
| Provider forward | `create_table(input)` | `input: table` | provider-defined | creates one table |
| Provider forward | `vector_upsert(input)` | `input: table` | provider-defined | upserts vector data |
| Provider forward | `vector_search(input)` | `input: table` | provider-defined | performs vector search |
| Provider forward | `delete(input)` | `input: table` | provider-defined | deletes records |
| Provider forward | `drop_table(input)` | `input: table` | provider-defined | drops one table |

### 15.2 Behavior Rules

- `enabled = true` means the current skill has a LanceDB binding.
- `info()` and `status()` always exist.
- When LanceDB is not enabled:
  - `enabled = false`
  - `info()` and `status()` return disabled-state descriptions
  - other methods error directly with `current skill has not enabled lancedb`

Stable development constraints:

- Pass one Lua table as `input`.
- Exact business fields and result shapes are defined by the LanceDB provider. LuaSkills only forwards the call and guards capability availability.
- The host/provider also receives stable binding context such as `space_label`, `binding_tag`, and `database_kind`.

### 15.3 Special Note

`vector_search(input)` results may contain two payload forms:

- `data_json`
- `data`

When the result format is JSON, the result table contains `data_json`; otherwise it contains the raw binary string `data`.

For exact input and output fields, combine the host LanceDB provider contract with:

- [Host database provider guide](zh-CN/providers/host-database-provider-guide.md)

## 16. Common Development Guidance

### 16.1 Probe Before Calling

For host-conditionally injected capabilities, probe first:

```lua
if vulcan.sqlite.enabled then
    return vulcan.sqlite.query_json({
        sql = "select 1 as ok",
    })
end

return {
    ok = false,
    reason = "sqlite-disabled",
}
```

### 16.2 Do Not Depend On Internal Field Names

These are internal mechanisms and should not be long-term skill dependencies:

- `vulcan.runtime.internal.*`
- `vulcan.__sqlite_skill_name`
- `vulcan.__lancedb_skill_name`

### 16.3 Do Not Guess Host Directory Layout

Prefer:

- `vulcan.context.skill_dir`
- `vulcan.context.entry_dir`
- `vulcan.context.entry_file`
- `vulcan.deps.*`

Do not infer:

- runtime root directory
- another skill's dependency directory
- whether the host uses a fixed directory name

### 16.4 Distinguish External Request Context From Internal Luaexec Context

If you see the real client name during normal skill execution but see `luaexec_call` inside `vulcan.runtime.lua.exec(...)`, that is expected by design, not a bug.

## 17. Recommended Reading Order

If you mainly write skills, read in this order:

1. This manual: understand the actual current `vulcan.*` capability surface.
2. [README.md](../README.md): understand runtime positioning and host boundaries.
3. [Host database provider guide](zh-CN/providers/host-database-provider-guide.md): understand SQLite / LanceDB host integration contracts.

If you are integrating a host rather than writing a skill, read:

- [FFI integration guide](zh-CN/ffi/integration-guide.md)
- [FFI host checklist](zh-CN/ffi/host-checklist.md)
