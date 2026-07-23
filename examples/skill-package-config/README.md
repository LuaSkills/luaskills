# Skill package configuration example

This example demonstrates one package-level configuration declaration shared by two entries.

中文完整说明见 [Skill 包级配置声明、运行时控制与宿主对接](../../docs/zh-CN/architecture/skill-config-system-design.md)。

The package covers:

- `integer`, `string`, `float`, `enum`, and `boolean`;
- numeric ranges and Unicode string-length limits;
- package-author-provided descriptions and enum metadata;
- defaults and one required sensitive value;
- `vulcan.config.describe/status`;
- one valid normalized write and one intentionally invalid write.

Expected flow:

1. Load `example-config-skill`.
2. Call `example-config-skill-status` to inspect completeness.
3. Use the host `runtime-config` wrapper with `action=describe`.
4. After host or user authorization, set `api_token`.
5. Call `example-config-skill-query`.
6. Pass `action=set_valid_demo` to persist `retry_count=5`.
7. Pass `action=set_invalid_demo` to observe range validation rejecting `99`.

Configuration belongs to the package. Both `query` and `status` use the same values.
Configuration metadata is single-language text chosen by the package author; English is recommended for broadly distributed packages but is not enforced.
