# Documentación de LuaSkills

[English](../index.md) | [简体中文](../zh-CN/index.md) | [日本語](../ja/index.md) | [한국어](../ko/index.md) | [Español](index.md) | [Français](../fr/index.md) | [Deutsch](../de/index.md) | [Português (BR)](../pt-BR/index.md)

[README en español](../../README.es.md) | [Por qué LuaSkills](product/why-luaskills.md)

La documentación se organiza por tarea: desarrollo de skills, integración con el host y SDK. Los documentos técnicos están en inglés; las referencias profundas de host y FFI se mantienen en chino.

## Por dónde empezar

| Quieres | Empieza aquí |
| --- | --- |
| Evaluar LuaSkills | [Por qué LuaSkills](product/why-luaskills.md) |
| Escribir un skill en Lua | [Skill development manual](../development/skill-development.md) |
| Escribir un System Plugin o flujos gestionados de Python/Node | [System Plugin managed runtime guide](../development/system-plugin-managed-runtime.md) |
| Integrar LuaSkills en un host Rust | [Runtime architecture](../integration/runtime-model.md) |
| Integrar mediante C ABI o JSON FFI | [FFI and integration overview](../integration/ffi-overview.md) |
| Usar TypeScript, Python o Go | [SDK overview](../sdk/index.md) |

## Desarrollo de skills

- [Skill development manual](../development/skill-development.md)
- [System Plugin managed runtime guide](../development/system-plugin-managed-runtime.md)
- [demo-skill](https://github.com/LuaSkills/demo-skill): plantilla mínima de skill.
- [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit): ejemplo de nivel de producción.

### Reglas de nombres de skills

`skill_id` y cada `entry.name` deben cumplir `^[a-z]([a-z0-9-]*[a-z0-9])?$`.
El nombre físico del directorio del skill es la única fuente de `skill_id`; `skill.yaml` no debe declarar un campo `skill_id`.
Las entradas canónicas se publican como `{skill_id}-{entry_name}` y pueden recibir un sufijo estable `-N` si hay conflictos.
En skills gestionados desde GitHub, el `skill_id` derivado o explícito, el prefijo del zip de release, el prefijo del checksum, el directorio superior del zip y el directorio instalado deben coincidir.
Los recursos de release usan `{skill_id}-v{version}-skill.zip` y `{skill_id}-v{version}-checksums.txt`; el zip debe contener `{skill_id}/skill.yaml`.

## Integración con el host

- [Runtime architecture](../integration/runtime-model.md)
- [FFI and integration overview](../integration/ffi-overview.md)
- [Host-selected managed runtime roots](../integration/managed-runtime-host-roots.md)
- [Package configuration host contract](../integration/package-configuration.md)
- [Database providers](../integration/database-providers.md)
- [Referencias detalladas en chino](../zh-CN/index.md#宿主对接)

## SDK

- [SDK overview](../sdk/index.md)
- [luaskills-sdk-typescript](https://github.com/LuaSkills/luaskills-sdk-typescript) · [luaskills-sdk-python](https://github.com/LuaSkills/luaskills-sdk-python) · [luaskills-sdk-go](https://github.com/LuaSkills/luaskills-sdk-go)

## Ejemplos locales

- [Demo de Rust](../../examples/demo-rust/README.md)
- [Demo C FFI](../../examples/ffi/c/README.md)
- [Demo TypeScript FFI](../../examples/ffi/typescript/README.md)
- [Standard Runtime Fixture](../../examples/ffi/standard_runtime/README.md)
- [Host Provider Demo](../../examples/ffi/host_provider_demo/README.md)
