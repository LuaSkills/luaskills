# Skill package configuration example

This example demonstrates one package-level configuration declaration shared by two entries.

中文完整说明见 [Skill 包级配置声明、运行时控制与宿主对接](../../docs/zh-CN/architecture/skill-config-system-design.md)。

The package covers:

- `integer`, `string`, `float`, `enum`, and `boolean`;
- numeric ranges and Unicode string-length limits;
- package-author-provided descriptions and enum metadata;
- host-facing titles, groups, ordering, formats, restart hints, and advanced flags;
- defaults and one required sensitive value;
- an isolated cross-field `config_validator`;
- `vulcan.config.describe/status`;
- atomic table writes, one valid normalized write, and one intentionally invalid write.

Expected flow:

1. Load `example-config-skill`.
2. Call `example-config-skill-status` to inspect completeness.
3. Use the host `runtime-config` wrapper with `action=describe`.
4. After host or user authorization, set `api_token`.
5. Call `example-config-skill-query`.
6. Pass `action=set_valid_demo` to atomically persist `retry_count=5` and `telemetry_enabled=true`.
7. Pass `action=set_invalid_demo` to observe range validation rejecting `99`.

Configuration belongs to the package. Both `query` and `status` use the same values.
Configuration metadata is single-language text chosen by the package author; English is recommended for broadly distributed packages but is not enforced.
