# Por Qué LuaSkills

[English](../../product/why-luaskills.md) | [简体中文](../../zh-CN/product/why-luaskills.md) | [日本語](../../ja/product/why-luaskills.md) | [한국어](../../ko/product/why-luaskills.md) | [Español](why-luaskills.md) | [Français](../../fr/product/why-luaskills.md) | [Deutsch](../../de/product/why-luaskills.md) | [Português (BR)](../../pt-BR/product/why-luaskills.md)

[Entrada en español](../index.md)

LuaSkills no solo resuelve cómo ejecutar Lua.
Resuelve cómo un producto puede organizar scripts, tools, bases de datos, workflows de AI Agent y extensiones instalables con una frontera de runtime estable.

## Problema De Producto

Muchos productos host terminan necesitando:

- Tools locales para AI Agents.
- Workflows para IDEs, aplicaciones de escritorio y herramientas de desarrollo.
- Skills de búsqueda, memoria o base de datos.
- Skills first-party incluidos con el producto.
- Skills instalables a nivel de project o user.

Lo difícil no es iniciar Lua; lo difícil es hacer que los skills encajen en una frontera de producto.

## Qué Aporta LuaSkills

LuaSkills proporciona un contrato de runtime, no una colección de convenciones.

- Carga de paquetes skill.
- Descubrimiento e invocación de entries.
- Strict help trees.
- Inyección de contexto de runtime.
- Inyección de rutas de dependencias.
- Routing de SQLite providers.
- Capas root de system, project y user.
- Integración Rust, C ABI y public `_json` FFI.

## Categorías De Capacidad

| Categoría | Qué habilita |
| --- | --- |
| Runtime Core | Cargar skills, recargar roots, listar entries y llamar skills. |
| Skill Authoring | Escribir skills con APIs `vulcan.*` y help estructurado. |
| Product Control | Mantener permisos, budget, UI y authority en el host. |
| Data-aware Skills | Conectar SQLite mediante runtime, host provider o space controller. |
| Multi-language Hosts | Usar el mismo modelo desde Rust, C ABI, TypeScript, Python y Go. |

## Ejemplos Importantes

- [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit): ejemplo product-grade de LuaSkills.
- [demo-skill](https://github.com/LuaSkills/demo-skill): template mínimo de repositorio skill.

## Conclusión

LuaSkills es una capa de runtime para convertir paquetes Lua en skills de producto.
Hace que los skills sean portables, deja el control al host y ofrece una ruta común para integraciones multilenguaje.
