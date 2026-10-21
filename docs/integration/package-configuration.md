# Package Configuration Host Contract

[Documentation hub](../index.md) | [Full Chinese design](../zh-CN/integration/skill-config-system-design.md)

Skill packages declare typed configuration in `skill.yaml`; LuaSkills validates, stores, and reports it; the host decides who may read or change it. The machine contract is [`contracts/skill-config/v1/contract.json`](../../contracts/skill-config/v1/contract.json), and every SDK generates its enums and limits from an exact copy of it.

## Package declaration

Configuration is declared only at the top level of `skill.yaml`. All entries in one package share the declaration and the persisted namespace.

```yaml
config:
  - key: retry_count
    type: integer
    description: Maximum request retry count
    default: 3
    constraints:
      minimum: 0
      maximum: 10

config_validator: runtime/config-validator.lua
```

- Supported types are `integer`, `string`, `float`, `enum`, and `boolean`.
- Human-readable metadata is single-language package content; there are no configuration-specific i18n fields or language fallback.
- The optional `config_validator` runs cross-field checks in an isolated, capability-free Lua state inside the same atomic write.
- `sensitive=true` hides values, not declarations. Never put a secret in `default`.

Lua code reads and writes its own package through `vulcan.config.get/has/list/status/delete` and `set(key, value)` or `set(table)`. LuaSkills binds these calls to the calling package, so one package cannot touch another. See the [skill development manual](../development/skill-development.md) for the Lua API.

## Host setup

1. Set `host_options.skill_config_root` to one explicit, absolute, writable, user-level directory.
2. LuaSkills keeps two fixed stores under it:
   - `<skill_config_root>/skills/config.json`
   - `<skill_config_root>/system-skills/config.json`
3. Writes send a typed, non-empty `values` object. Single-key SDK helpers wrap the same batch transaction.
4. Every `revision` and event `sequence` is a decimal string.
5. Use `expected_revision` for compare-and-swap writes and deletes whenever UI state can be stale or several processes share the store.

SDK and FFI configuration calls are cross-package management APIs. The embedding product must authorize them.

## Persisted format

```json
{
  "format_version": 1,
  "revision": "1",
  "skills": {
    "example-skill": {
      "enabled": "true",
      "retry_count": "3"
    }
  }
}
```

Unversioned files are rejected; create the document explicitly or let LuaSkills create it on the first write. External editors must take the companion `.lock`, read the latest document while holding it, increment `revision`, write a temporary file in the same directory, flush it, and atomically replace the destination. Invalid or non-monotonic external files never replace the last good cached snapshot.

## Discovery, disclosure, and events

- `describe` returns declared types, constraints, UI hints, defaults, states, completeness, and orphaned keys.
- Installed discovery scans enabled, disabled, shadowed, and invalid packages without running Lua.
- Saved and effective values are omitted unless the host passes `include_values=true`.
- LuaSkills does not decide who may read or change values; the host may deny, force parameters, or ask the user.
- File-watch events are ordered by an engine-local decimal `sequence`. Process a complete page before advancing to `next_sequence`.
- Uninstall does not delete configuration; explicit cleanup belongs to the host.

## Missing configuration

When `status.complete` is false, a skill should tell the host or the AI to call `runtime-config` with `action=describe` and its package id. If changes are authorized, submit all available values in one `set` batch; otherwise list the exact declared parameters the user must provide, without echoing secrets.
