# Runtime Architecture Overview

[Documentation hub](../index.md) | [Chinese root policy](../zh-CN/architecture/skill-root-layer-policy.md)

LuaSkills is intentionally a runtime layer, not a full product host.
Its architecture separates skill execution truth from product policy.

## Runtime Responsibilities

LuaSkills owns:

- Skill package loading.
- Entry discovery and invocation.
- Strict help parsing.
- Runtime result shaping.
- `vulcan.*` and `vulcan.runtime.*` injection.
- Standard `vulcan.models.*` API shape, argument validation, result envelopes, and caller-context forwarding.
- Skill dependency path injection.
- Optional SQLite and LanceDB provider routing.
- Lifecycle primitives such as install, update, enable, disable, and uninstall.

## Host Responsibilities

The host owns:

- User permissions.
- Product UI and command surfaces.
- Client budgets and truncation policy.
- Configuration source and storage placement.
- Model provider configuration, API keys, routing, budgets, redaction, and callback registration.
- Tool projection rules, including managed identity field hiding, injection, and result redaction.
- Which roots are writable.
- Which authority is attached to each management operation.
- Whether database ownership stays local, host-controlled, or controller-backed.

## Managed Identity Field Boundary

LuaSkills reserves `LUASKILL_SID` as the standard entry argument for skills that need stable session, task, or context identity. This field belongs to the LuaSkills skill/host contract, not to any single transport such as MCP.

LuaSkills owns:

- The reserved argument name `LUASKILL_SID`.
- The expectation that stateful skills use this name when they need caller-visible or host-managed identity.
- The create/start/bootstrap guidance for explicit reuse, non-managed fallback generation, public identity return, and authorized persistence prompts.

The runtime treats `LUASKILL_SID` as ordinary entry input. It does not automatically hide, inject, persist, or redact the value.

The host owns:

- Detecting `LUASKILL_SID` in entry schemas when projecting entries into tools.
- Hiding the field from model/user-facing schemas when the host can provide stable managed identity.
- Injecting the stable identity before calling the entry.
- Prefixing host-managed injected identities with the reserved `LUASKILLS-SID-` marker when skills need a portable way to hide host-owned values in results.
- Leaving the field visible when no stable managed identity exists.
- Rewriting help and redacting results in managed mode so raw managed identities are not exposed accidentally.

This keeps LuaSkills portable across MCP, gRPC, FFI/SDK, IDE, and embedded hosts while giving each host room to bind the identity to its own conversation, task, workspace, or product session model.

## Managed Project-Path Boundary

LuaSkills also reserves `PWD` as the conventional entry argument name for skills that need one caller-visible project or workspace path aligned with the host's active project scope.

LuaSkills owns:

- The conventional argument name `PWD`.
- The expectation that project-aware skills use this name when they need one host- or caller-provided project/workspace root.
- The expectation that `PWD` means one portability-oriented project/workspace path contract, not one authority token or guaranteed raw process cwd.

The runtime treats `PWD` as ordinary entry input. It does not automatically hide, inject, normalize, or rewrite the value.

The host owns:

- Detecting `PWD` in entry schemas when projecting entries into tools.
- Hiding the field from model/user-facing schemas when the host can provide one stable current project/workspace path.
- Injecting the current project/workspace path before calling the entry.
- Leaving the field visible when no stable project/workspace path exists.
- Making it clear in managed mode that the path is host-supplied and should not be requested from the user.

This keeps LuaSkills portable across MCP, gRPC, FFI/SDK, IDE, and embedded hosts while letting each host map `PWD` to its own project, workspace, or product root model. It is a compatibility convention, not one runtime hard restriction.

## Standard Model Capability Boundary

`vulcan.models.*` is part of the LuaSkills standard runtime surface, but the real model implementation is always host-owned.
LuaSkills only provides:

- The fixed Lua API: `status`, `has`, `embed`, and `llm`.
- Argument validation for the standard API.
- Table-shaped success and error envelopes.
- Caller-context forwarding to host callbacks.
- Capability discovery based on whether the matching callback is registered.

The host provides:

- The provider configuration, API keys, base URL, model names, temperature, thinking, stream policy, timeout, and budgets.
- The dedicated callback for each enabled capability.
- Provider error redaction before returning provider details to LuaSkills.
- Audit, rate limiting, and cost attribution based on caller context.

Model configuration is intentionally separate from `skill_config`.
Lua skills cannot read, set, or override provider configuration through `vulcan.models.*`.

## Model Callback Integration Flow

Use this sequence for hosts that expose model capabilities:

1. Load host-managed model configuration from the host's own source, such as `model_config.yaml`, environment, workspace policy, or product settings.
2. Register `embed` and `llm` callbacks separately. Register only the capabilities the host wants to enable.
3. Create the LuaSkills engine and load skill roots.
4. Invoke skills normally. Lua code can discover model capability state with `vulcan.models.status()` or `vulcan.models.has(capability)`.
5. Clear callbacks during host shutdown when using process-level FFI or SDK callback registration.

When a callback is missing, the corresponding Lua method returns a structured `model_unavailable` error.
It does not require the Lua skill to wrap the call in `pcall`.

## JSON FFI And SDK Request Shapes

JSON FFI callbacks and SDK callbacks receive fixed request payloads.
Embedding callbacks receive:

```json
{
  "text": "hello",
  "caller": {
    "skill_id": "demo-skill",
    "entry_name": "ask",
    "canonical_tool_name": "demo-skill-ask",
    "root_name": "USER",
    "skill_dir": "D:/runtime/luaskills/user_skills/demo-skill",
    "client_name": "mcp-host",
    "request_id": "req-123"
  }
}
```

LLM callbacks receive:

```json
{
  "system": "You are concise.",
  "user": "Summarize this note.",
  "caller": {
    "skill_id": "demo-skill",
    "entry_name": "summarize",
    "canonical_tool_name": "demo-skill-summarize",
    "root_name": "USER",
    "skill_dir": "D:/runtime/luaskills/user_skills/demo-skill",
    "client_name": "mcp-host",
    "request_id": "req-124"
  }
}
```

Success responses are bare model payloads:

```json
{
  "vector": [0.1, 0.2, 0.3],
  "dimensions": 3,
  "usage": {
    "input_tokens": 12
  }
}
```

```json
{
  "assistant": "summary text",
  "usage": {
    "input_tokens": 12,
    "output_tokens": 8
  }
}
```

Provider failures should use the standard error envelope:

```json
{
  "ok": false,
  "error": {
    "code": "provider_error",
    "message": "model provider rejected the request",
    "provider_message": "raw provider message after host-side redaction",
    "provider_code": "model_not_found",
    "provider_status": 404
  }
}
```

`provider_message`, `provider_code`, and `provider_status` are optional, but hosts should preserve them whenever they are safe to expose after redaction.

## SDK Integration Map

| Host | Recommended Entry | Model Callback Notes |
| --- | --- | --- |
| Rust | Direct crate API | Use `set_model_embed_callback` and `set_model_llm_callback` with typed request and response structs. |
| TypeScript / Node.js | `@luaskills/sdk` | Use `LuaSkillsJsonFfi.setModelEmbedJsonCallback` and `setModelLlmJsonCallback`, then create/load the client. |
| Python | `luaskills-sdk` | Use `LuaSkillsJsonFfi.set_model_embed_json_callback` and `set_model_llm_json_callback`, then create/load the client. |
| Go | `luaskills-sdk-go` or C ABI | The SDK exposes typed model request/response/error shapes, but direct process-level callback registration still requires a host-owned cgo bridge. |

## Skill Root Layers

The active model uses three conceptual root layers:

```text
ROOT -> PROJECT -> USER
```

- `ROOT` is the system-controlled layer for host-managed core skills.
- `PROJECT` is the project-level writable layer.
- `USER` is the user-level writable layer.

When the same `skill_id` exists in multiple layers, higher-priority layers win.
`ROOT` is special: ordinary user management flows should not mutate it.

## Help Model

LuaSkills treats help as structured runtime data, not as ordinary tool output.

The host can render help as:

- Markdown documentation.
- Tool descriptions.
- Command palette entries.
- UI panels.
- Agent context.

## Deep References

- [System Plugin managed runtime guide](../system-plugin-managed-runtime.md)
- [System Plugin 受管运行时使用指南](../zh-CN/system-plugin-managed-runtime.md)
- [Skill root layer policy](../zh-CN/architecture/skill-root-layer-policy.md)
- [Skill package configuration declaration and host integration](../zh-CN/architecture/skill-config-system-design.md)
- [Lua Skill developer manual](../skill-development.md)
- [Chinese Lua Skill developer manual](../zh-CN/skill-development.md)
