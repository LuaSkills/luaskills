# LuaSkills Documentation

[English](index.md) | [简体中文](zh-CN/index.md) | [日本語](ja/index.md) | [한국어](ko/index.md) | [Español](es/index.md) | [Français](fr/index.md) | [Deutsch](de/index.md) | [Português (BR)](pt-BR/index.md)

[Repository README](../README.md) | [Why LuaSkills](product/why-luaskills.md)

Documentation is grouped by what you are building. Deep host and FFI references are maintained in Chinese and linked where no English edition exists yet.

## Choose your path

| You are | Start here |
| --- | --- |
| Evaluating LuaSkills | [Why LuaSkills](product/why-luaskills.md) |
| Writing a Lua skill | [Skill development manual](development/skill-development.md) |
| Writing a System Plugin or managed Python/Node workflow | [System Plugin managed runtime guide](development/system-plugin-managed-runtime.md) |
| Embedding LuaSkills in a Rust host | [Runtime architecture](integration/runtime-model.md) |
| Embedding through the C ABI or JSON FFI | [FFI and integration overview](integration/ffi-overview.md) |
| Using TypeScript, Python, or Go | [SDK overview](sdk/index.md) |

## Skill development

For skill and System Plugin authors.

- [Skill development manual](development/skill-development.md): package layout, `skill.yaml`, entries, input schemas, and every `vulcan.*` API. [中文](zh-CN/development/skill-development.md)
- [System Plugin managed runtime guide](development/system-plugin-managed-runtime.md): strict System Plugin packages, persistent Python/Node sessions, events, and cleanup. [中文](zh-CN/development/system-plugin-managed-runtime.md)
- [demo-skill](https://github.com/LuaSkills/demo-skill): minimal skill template.
- [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit): production-grade skill example.

### Skill naming rules

`skill_id` and every `entry.name` must match `^[a-z]([a-z0-9-]*[a-z0-9])?$`.
The physical skill directory name is the only `skill_id`; `skill.yaml` must not declare a `skill_id` field.
Canonical entries are exposed as `{skill_id}-{entry_name}` and may receive a stable `-N` suffix on conflicts.
For GitHub-managed skills, the repository-derived or explicit `skill_id`, release zip prefix, checksum prefix, zip top-level directory, and installed directory must be identical.
Use `{skill_id}-v{version}-skill.zip`, `{skill_id}-v{version}-checksums.txt`, and a zip containing `{skill_id}/skill.yaml`.

## Host integration

For products that embed the runtime.

- [Runtime architecture](integration/runtime-model.md): host and runtime responsibilities, managed identity and project-path fields, model callbacks, skill root layers, and help.
- [FFI and integration overview](integration/ffi-overview.md): integration layers, the first integration sequence, and key FFI rules.
- [Host-selected managed runtime roots](integration/managed-runtime-host-roots.md): independent data, distribution, and environment roots, resource policy, and resolvers. [中文](zh-CN/integration/managed-runtime-host-roots.md)
- [Package configuration host contract](integration/package-configuration.md): declarations, stores, revisions, disclosure, and events.
- [Database providers](integration/database-providers.md): SQLite and LanceDB ownership modes.
- Chinese references:
  - [FFI integration guide](zh-CN/integration/ffi-integration-guide.md)
  - [FFI host checklist](zh-CN/integration/ffi-host-checklist.md)
  - [Host database provider guide](zh-CN/integration/host-database-provider-guide.md)
  - [Skill root layer policy](zh-CN/integration/skill-root-layer-policy.md)
  - [Skill sources, official hub, and progress events](zh-CN/integration/skill-source-policy-and-hub.md)
  - [Package configuration design](zh-CN/integration/skill-config-system-design.md)
  - [Host tooling result bridge and `system_lua_lib`](zh-CN/integration/host-tooling-result-bridge-design.md)
- Machine contracts: [embedded runtime](../contracts/embedded-runtime.md), [embedded FFI](../contracts/embedded-ffi.md), [System lease execution](../contracts/system-lease-execution.md).

## SDKs

- [SDK overview](sdk/index.md): packages, coverage, version rules, and release order.
- [luaskills-sdk-typescript](https://github.com/LuaSkills/luaskills-sdk-typescript)
- [luaskills-sdk-python](https://github.com/LuaSkills/luaskills-sdk-python)
- [luaskills-sdk-go](https://github.com/LuaSkills/luaskills-sdk-go)

## Local examples

- [Rust demo](../examples/demo-rust/README.md): direct crate integration with `call_skill` and `vulcan.host.*`.
- [Managed Python/Node runtime example](../examples/managed_runtime/README.md)
- [Skill package configuration example](../examples/skill-package-config/README.md)
- [C FFI demo](../examples/ffi/c/README.md)
- [TypeScript FFI demo](../examples/ffi/typescript/README.md)
- [Standard runtime fixture](../examples/ffi/standard_runtime/README.md)
- [FFI demo runtime](../examples/ffi/demo_runtime/README.md)
- [Host provider demo](../examples/ffi/host_provider_demo/README.md)
- [FFI demo package](../examples/demo-ffi/README.md)
- `cargo run --bin luaskills-debug -- inspect --runtime-root <dir> --skill-path <dir>`: syncs one skill into a real `runtime_root` and loads it the way a host would.

## Maintainers

- [Release workflow](../scripts/release/README.md)
- [Contributing and DCO](../CONTRIBUTING.md)
