# Database Provider Overview

[Documentation hub](../index.md) | [Chinese provider guide](../zh-CN/integration/host-database-provider-guide.md)

LuaSkills supports database-aware skills through SQLite and LanceDB bindings.
The important product question is not only how a skill queries a database, but who owns database placement, sharing, lifecycle, and concurrency.

## Provider Modes

Each backend can choose its own provider mode.

| Mode | Meaning |
| --- | --- |
| `dynamic_library` | LuaSkills loads local database backend libraries and calls them directly. |
| `host_callback` | LuaSkills sends database requests back to the host through registered callbacks. |
| `space_controller` | LuaSkills forwards database requests to an external space controller service. |

SQLite and LanceDB can use different modes in the same host.
For example, SQLite can use `host_callback + json` while LanceDB uses `dynamic_library`.

## Callback Modes

When provider mode is `host_callback`, the host also chooses a callback transport:

- `standard`: structured C ABI callbacks for hosts that can handle explicit structs and buffers.
- `json`: JSON callback payloads for dynamic language hosts and SDK-style integrations.

## Why Host Callback Exists

Memory and database skills are stateful.
If multiple host processes open the same physical database directly, ownership and concurrency become product problems.

Host callback mode lets LuaSkills keep a stable skill-facing API while the host controls:

- Database location.
- Sharing policy.
- Multi-process access.
- Tenant or workspace mapping.
- Migration and backup strategy.

## Recommended Reading

- [Host database provider guide](../zh-CN/integration/host-database-provider-guide.md)
- [FFI integration guide](../zh-CN/integration/ffi-integration-guide.md)
- [Host provider demo](../../examples/ffi/host_provider_demo/README.md)
