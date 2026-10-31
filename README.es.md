<div align="center">

# LuaSkills

**Un runtime en Rust que convierte scripts Lua en skills de calidad de producto**

Núcleo en Rust · skills en LuaJIT · Rust API, C ABI, JSON FFI y SDK de TypeScript / Python / Go

[![crates.io](https://img.shields.io/crates/v/luaskills.svg)](https://crates.io/crates/luaskills)
[![docs.rs](https://img.shields.io/docsrs/luaskills)](https://docs.rs/luaskills)
[![License: MIT](https://img.shields.io/badge/License-MIT-green.svg)](LICENSE)
[![Release](https://img.shields.io/github/v/release/LuaSkills/luaskills)](https://github.com/LuaSkills/luaskills/releases)

[English](README.md) | [简体中文](README.zh-CN.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | **Español** | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (BR)](README.pt-BR.md)

[Documentación](docs/es/index.md) · [Plantilla de skill](https://github.com/LuaSkills/demo-skill) · [Ejemplo CodeKit](https://github.com/LuaSkills/vulcan-codekit)

</div>

> Esta es una traducción del [README en inglés](README.md). Si difieren, prevalece la versión en inglés.

---

## Acerca del proyecto

LuaSkills carga, ejecuta y gestiona paquetes de skills en Lua dentro de una aplicación host. El host obtiene herramientas programables, ayuda estructurada, runtimes gestionados de Python y Node.js, gestión de dependencias y ejecución con acceso a bases de datos, sin construir su propio runtime de plugins.

**LuaSkills ejecuta skills; el host decide cómo se convierten en funciones de producto.** Las políticas, la autenticación, las confirmaciones de permisos, los presupuestos, la ubicación del almacenamiento y la experiencia de usuario siguen en manos del host.

LuaSkills impulsa el sistema de skills de [Vulcan Code](https://github.com/OpenVulcan/vulcan-code). Encaja en cualquier agente de IA, IDE o producto de escritorio o servidor que necesite un sistema de skills controlado en lugar de scripts embebidos aislados.

## Características

### 🧩 Runtime de skills

- Descubrir, cargar, listar, invocar y recargar skills en raíces por capas `ROOT`, `PROJECT` y `USER`
- Árboles de ayuda estrictos que el host muestra como documentación, descripciones de herramientas, paletas de comandos o paneles
- Un esquema de entrada de objeto completo por entrada, con nombres canónicos `{skill_id}-{entry_name}`
- Instalar, actualizar y desinstalar desde releases de GitHub o URL, con dependencias verificadas por checksum

### 🧰 APIs estándar de Lua

- `vulcan.fs`, `vulcan.path`, `vulcan.process`, `vulcan.io`, `vulcan.os`, `vulcan.json`, `vulcan.cache`
- `vulcan.context` con datos de solicitud, cliente, presupuesto y directorios del skill; `vulcan.deps` para las raíces de dependencias
- `vulcan.host.*` llama a herramientas registradas por el host; `vulcan.models.*` usa modelos de embeddings y LLM gestionados por el host
- `vulcan.config.*` ofrece a cada paquete configuración tipada y validada, con almacenes versionados y eventos de cambio

### 🐍 Python y Node.js gestionados

- Los skills invocan manejadores versionados de Python y Node.js mediante `vulcan.runtime.python.*` y `vulcan.runtime.node.*`
- Dependencias bloqueadas, salida acotada y limpieza determinista del árbol de procesos
- Sesiones persistentes entre llamadas para leases estrictos de System Plugin, con eventos del host y un callback de activación por flanco
- Raíces de distribución de solo lectura y raíces de entorno escribibles elegidas por el host

### 🔌 Integración con el host

- API del crate para hosts Rust
- C ABI estándar y FFI pública `_json` para cualquier otro lenguaje
- `runtime_lease` público y `system_runtime_lease` ligado a autoridad para VMs de Lua persistentes
- Resultados estructurados `host_result`, empezando por ediciones `change_set` de nivel IDE
- SQLite opcional mediante callbacks registrados por el host

### 📦 SDK

- SDK de [TypeScript / Node.js](https://github.com/LuaSkills/luaskills-sdk-typescript), [Python](https://github.com/LuaSkills/luaskills-sdk-python) y [Go](https://github.com/LuaSkills/luaskills-sdk-go) sobre la FFI JSON
- Instalación de recursos del runtime con verificación SHA-256

## Lo que LuaSkills no es

- No es un servidor MCP por sí mismo; [vulcan-mcp](https://github.com/OpenVulcan/vulcan-mcp) aporta una capa de host MCP
- No lee la configuración del host, no calcula presupuestos ni renderiza interfaces
- **No es un sandbox.** Los skills son código de confianza. El host decide qué raíces se activan, qué skills se instalan, qué acciones de gestión se exponen, qué modos de base de datos se permiten y qué autoridad lleva cada operación.

## Arquitectura

```
┌──────────────────────────────┐         ┌──────────────────────────────────────┐
│  Host application            │  Rust   │  LuaSkills runtime                   │
│  policy · UI · permissions   │  C ABI  │  skill loader · help · lifecycle     │
│  budgets · model providers   │◀───────▶│  LuaJIT VMs · leases · vulcan.* APIs │
│  host tools · storage        │  JSON   │  managed Python / Node · deps        │
│                              │  FFI    │  config stores · SQLite    │
└──────────────────────────────┘         └──────────────────────────────────────┘
                                                          │
                                              skills/<skill_id>/skill.yaml
```

| Directorio | Descripción |
| --- | --- |
| `src/runtime` | Motor, VMs de Lua, leases, contexto, ayuda, resultados y runtimes gestionados |
| `src/skill` | Manifiestos, raíces, configuración y gestión del ciclo de vida |
| `src/dependency`, `src/download` | Resolución de dependencias, descargas y extracción de archivos |
| `src/providers` | Enlaces de SQLite |
| `src/ffi`, `src/ffi_standard`, `src/ffi_embedded` | FFI pública `_json`, C ABI estándar y transporte del runtime embebido |
| `include/` | Cabeceras C para hosts FFI |
| `contracts/` | Contratos de máquina compartidos con los SDK |
| `examples/` | Ejemplos de Rust, FFI, runtimes gestionados y configuración |

## Primeros pasos

### Hosts Rust

```toml
[dependencies]
luaskills = "0.7.4"
```

[`examples/demo-rust`](examples/demo-rust/README.md) muestra `call_skill` y el puente de herramientas `vulcan.host.*`.

### Otros lenguajes

| Host | Opción |
| --- | --- |
| TypeScript / Node.js | `npm install @luaskills/sdk` |
| Python | `pip install luaskills-sdk` |
| Go | `go get github.com/LuaSkills/luaskills-sdk-go` |
| C / C++ | Descarga `luaskills-ffi-sdk-{platform}.tar.gz` desde [Releases](https://github.com/LuaSkills/luaskills/releases) y usa la C ABI estándar |

Consulta el [SDK overview](docs/sdk/index.md) y el [FFI overview](docs/integration/ffi-overview.md).

### Escribir un skill

Empieza con [demo-skill](https://github.com/LuaSkills/demo-skill) y lee el [Skill development manual](docs/development/skill-development.md). Para depurar un skill con una estructura de runtime real:

```bash
cargo run --features debug-sqlite --bin luaskills-debug -- call \
  --runtime-root D:/runtime \
  --skill-path D:/skills/demo-skill \
  --tool ping \
  --args-json "{\"note\":\"hello\"}"
```

`luaskills-debug` copia el skill a `runtime_root/skills/<skill_id>` y ejecuta la ruta normal `load_from_roots -> call_skill`, de modo que raíces de dependencias, estado, bases de datos y contexto coinciden con la ejecución real del host. Es una herramienta de desarrollo del repositorio y no añade API al SDK ni a la FFI.

## Documentación

| Tema | Documentos |
| --- | --- |
| Desarrollo de skills | [Skill development manual](docs/development/skill-development.md) · [System Plugin managed runtime guide](docs/development/system-plugin-managed-runtime.md) |
| Integración con el host | [Runtime architecture](docs/integration/runtime-model.md) · [FFI overview](docs/integration/ffi-overview.md) · [Managed runtime roots](docs/integration/managed-runtime-host-roots.md) · [Package configuration](docs/integration/package-configuration.md) · [Database providers](docs/integration/database-providers.md) |
| SDK | [SDK overview](docs/sdk/index.md) |
| Todo | [Documentación en español](docs/es/index.md) · [Documentation hub](docs/index.md) |

## Ecosistema

| Repositorio | Descripción |
| --- | --- |
| [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit) | Navegación de código, inspección AST, búsqueda estructural, navegación Markdown y parches seguros |
| [vulcan-curl](https://github.com/LuaSkills/vulcan-curl) | Solicitudes HTTP GET / POST estructuradas |
| [vulcan-file](https://github.com/LuaSkills/vulcan-file) | Listados que respetan reglas de ignorado, lecturas exactas y pequeñas ediciones con vista previa |
| [vulcan-lua](https://github.com/LuaSkills/vulcan-lua) | Ejecución acotada de Lua en línea o desde archivos |
| [vulcan-testkit](https://github.com/LuaSkills/vulcan-testkit) | Convierte la salida de build, tests, lint y verificación de tipos en diagnósticos compactos |
| [vulcan-workmem](https://github.com/LuaSkills/vulcan-workmem) | Memoria de trabajo por proyecto para puntos de control y traspasos |
| [demo-skill](https://github.com/LuaSkills/demo-skill) | Plantilla mínima de repositorio de skill |
| [luaskills-packages](https://github.com/LuaSkills/luaskills-packages) | Paquetes del runtime de Lua y dependencias nativas |

## Versiones

| Componente | Versión |
| --- | --- |
| Núcleo de LuaSkills y FFI SDK | `0.7.4` |
| SDK de TypeScript, Python y Go | Líneas de versión independientes; ver [SDK overview](docs/sdk/index.md#versions) |
| Paquetes del runtime de Lua | serie compatible `0.1` |
| Python / uv gestionados | `3.14.6` / `0.11.28` |
| Node.js / pnpm gestionados | `24.18.0` / `11.11.0` |
| Valores por defecto del runtime gestionado | `workers=4 / idle=60s / sessions=256 / buffer=1MiB/stream / invoke=unlimited` |

Los valores por defecto de los scripts están en `scripts/deps/`. Las publicaciones siguen el [flujo de release](scripts/release/README.md): primero los paquetes del runtime, luego el release del núcleo en cinco plataformas, después el crate y por último los SDK.

## Contribuir

Issues y pull requests son bienvenidos.

1. Haz un fork del repositorio y crea una rama desde `main`.
2. Verifica en local:

   ```bash
   cargo fmt --all -- --check
   cargo clippy --all-targets -- -D warnings
   cargo test --all-targets -- --test-threads=1
   cargo deny check
   ```

3. Firma cada commit con `git commit -s`; consulta [CONTRIBUTING.md](CONTRIBUTING.md) para el DCO.
4. Mantén cada pull request centrado en un solo cambio y explica qué cambiaste, por qué y cómo lo verificaste.

## Licencia

[MIT](LICENSE). Los avisos de terceros están en [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
