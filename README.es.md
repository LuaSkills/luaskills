# LuaSkills

[English](README.md) | [简体中文](README.zh-CN.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (BR)](README.pt-BR.md)

[Documentación](docs/es/index.md) | [Plantilla de skill](https://github.com/LuaSkills/demo-skill) | [Ejemplo CodeKit](https://github.com/LuaSkills/vulcan-codekit)

LuaSkills es un runtime escrito en Rust para cargar, ejecutar y administrar skills basadas en Lua.
Permite que una aplicación host agregue herramientas scriptables, help estructurado, APIs de runtime, rutas de dependencias e integración con SQLite / LanceDB sin construir su propio runtime de plugins desde cero.

En una frase: LuaSkills ejecuta skills; el host decide cómo convertirlas en funciones de producto.

## Qué Es

LuaSkills es la capa de runtime central del ecosistema LuaSkills.
Está diseñado para aplicaciones que necesitan un sistema de skills controlado, no scripts sueltos.

Proporciona:

- Descubrimiento, carga, enumeración e invocación de skills.
- Strict help trees que el host puede mostrar como documentación, comandos, tools o UI.
- Inyección de APIs estándar `vulcan.*` y `vulcan.runtime.*`.
- Sesiones persistentes de Python y Node.js aisladas por paquete para skills y System Plugins en Windows x64, Linux x64/ARM64 y macOS x64/ARM64; Windows ARM no está soportado explícitamente.
- Contexto de runtime para request actual, directorios de skill, recursos, dependencias y metadatos del cliente.
- Integración opcional con SQLite y LanceDB para skills con estado o memoria.
- API Rust, C ABI estándar y FFI pública `_json`.
- Rutas de integración SDK para TypeScript, Python y Go.

## Qué No Es

LuaSkills no controla toda la superficie del producto.

No es:

- Un MCP server por sí mismo.
- Un lector de configuración del host.
- Un calculador de budget del cliente.
- Un renderer de UI de producto.
- Una frontera sandbox para código Lua no confiable.

El host sigue controlando permisos, autenticación, UI, budgets, almacenamiento y exposición al usuario.

## Ecosistema

- [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit): ejemplo importante y cercano a producción que muestra navegación de código, inspección AST, búsqueda estructural, navegación Markdown y workflows de patch.
- [vulcan-curl](https://github.com/LuaSkills/vulcan-curl): skill de requests HTTP con entradas GET / POST estructuradas y ejecución de requests estilo curl.
- [vulcan-file](https://github.com/LuaSkills/vulcan-file): skill de operaciones de archivo para listar con reglas de ignore, leer texto exacto y hacer ediciones pequeñas con vista previa.
- [vulcan-lua](https://github.com/LuaSkills/vulcan-lua): skill de ejecución Lua controlada para código inline o tareas basadas en archivos Lua.
- [vulcan-testkit](https://github.com/LuaSkills/vulcan-testkit): router de validación que convierte salidas de build, test, lint y typecheck en diagnósticos compactos.
- [vulcan-workmem](https://github.com/LuaSkills/vulcan-workmem): skill de memoria de trabajo por proyecto para checkpoints de tarea y contexto de handoff persistente.
- [demo-skill](https://github.com/LuaSkills/demo-skill): plantilla mínima para aprender `skill.yaml`, runtime entries, help y layout de repositorio.
- [luaskills-sdk-typescript](https://github.com/LuaSkills/luaskills-sdk-typescript): SDK para TypeScript / Node.js.
- [luaskills-sdk-python](https://github.com/LuaSkills/luaskills-sdk-python): SDK para Python.
- [luaskills-sdk-go](https://github.com/LuaSkills/luaskills-sdk-go): SDK para Go.

## Documentación

- [Entrada en español](docs/es/index.md)
- [Por Qué LuaSkills](docs/es/product/why-luaskills.md)
- [Entrada en inglés](docs/index.md)
- [Documentación técnica detallada en chino](docs/zh-CN/index.md)

## Rutas De Integración

| Tipo de host | Ruta recomendada |
| --- | --- |
| Rust | Usar directamente el crate Rust. |
| C / C++ / host de bajo nivel | Usar el C ABI estándar. |
| TypeScript / Node.js | Preferir `luaskills-sdk-typescript`. |
| Python | Preferir `luaskills-sdk-python`. |
| Go | Elegir `luaskills-sdk-go` o C ABI estándar según callbacks y despliegue. |

## Inicio Rápido

Host Rust:

```toml
[dependencies]
luaskills = "0.5"
```

Comandos de desarrollo:

```bash
cargo check
cargo test --lib
```

Para aprender la forma de un skill:

1. [demo-skill](https://github.com/LuaSkills/demo-skill)
2. [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit)
3. [Skill development manual](docs/skill-development.md)

## Reglas De Nomenclatura De Skills

`skill_id` y cada `entry.name` deben cumplir `^[a-z]([a-z0-9-]*[a-z0-9])?$`.
El nombre físico del directorio del skill es la única fuente de `skill_id`; `skill.yaml` no debe declarar un campo `skill_id`.
Las entradas canonical usan `{skill_id}-{entry_name}` y pueden recibir un sufijo estable `-N` si hay conflictos.
Para skills gestionados por GitHub, el `skill_id` derivado del repositorio o explícito, el prefijo del zip de release, el prefijo de checksums, el directorio superior del zip y el directorio instalado deben ser idénticos.
Los assets usan `{skill_id}-v{version}-skill.zip` y `{skill_id}-v{version}-checksums.txt`; el zip debe contener `{skill_id}/skill.yaml`.

## Modelo De Confianza

Actualmente LuaSkills trata los skills como código confiable por defecto.
No ofrece una promesa de sandbox para paquetes Lua arbitrarios y no confiables.

El host debe decidir roots, skills instalables, acciones de gestión, modo de database provider y authority de cada operación.

## Matriz De Versiones Actual

| Componente | Versión actual |
| --- | --- |
| Núcleo LuaSkills, FFI SDK y línea de SDK de lenguajes | `0.5.0` |
| Serie compatible de paquetes del runtime Lua | `0.1` |
| Python / uv gestionados | `3.12.7` / `0.11.17` |
| Node.js / pnpm gestionados | `22.11.0` / `9.15.0` |
| vldb-controller | `0.2.1` |
| vldb-sqlite / vldb-lancedb | `0.1.5` / `0.1.5` |

Los valores ejecutables predeterminados se definen en `scripts/deps/fetch_managed_runtimes.*`, `scripts/deps/fetch_deps.*` y `scripts/deps/sync_runtime_assets.*`. `dependencies.yaml` debe declarar estas versiones exactas del runtime y del gestor de paquetes, salvo que el host instale y declare deliberadamente otra versión exacta compatible.

## License

MIT
