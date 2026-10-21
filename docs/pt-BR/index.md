# Documentação do LuaSkills

[English](../index.md) | [简体中文](../zh-CN/index.md) | [日本語](../ja/index.md) | [한국어](../ko/index.md) | [Español](../es/index.md) | [Français](../fr/index.md) | [Deutsch](../de/index.md) | [Português (BR)](index.md)

[README em português](../../README.pt-BR.md) | [Por que LuaSkills](product/why-luaskills.md)

A documentação é organizada por tarefa: desenvolvimento de skills, integração com o host e SDKs. Os documentos técnicos estão em inglês; as referências aprofundadas de host e FFI são mantidas em chinês.

## Por onde começar

| Você quer | Comece aqui |
| --- | --- |
| Avaliar o LuaSkills | [Por que LuaSkills](product/why-luaskills.md) |
| Escrever uma skill em Lua | [Skill development manual](../development/skill-development.md) |
| Escrever um System Plugin ou fluxos gerenciados de Python/Node | [System Plugin managed runtime guide](../development/system-plugin-managed-runtime.md) |
| Incorporar o LuaSkills em um host Rust | [Runtime architecture](../integration/runtime-model.md) |
| Incorporar via C ABI ou JSON FFI | [FFI and integration overview](../integration/ffi-overview.md) |
| Usar TypeScript, Python ou Go | [SDK overview](../sdk/index.md) |

## Desenvolvimento de skills

- [Skill development manual](../development/skill-development.md)
- [System Plugin managed runtime guide](../development/system-plugin-managed-runtime.md)
- [demo-skill](https://github.com/LuaSkills/demo-skill): modelo mínimo de skill.
- [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit): exemplo de nível de produção.

### Regras de nomes de skills

`skill_id` e cada `entry.name` devem corresponder a `^[a-z]([a-z0-9-]*[a-z0-9])?$`.
O nome físico do diretório da skill é a única fonte de `skill_id`; `skill.yaml` não deve declarar o campo `skill_id`.
Entradas canônicas são publicadas como `{skill_id}-{entry_name}` e podem receber um sufixo estável `-N` em caso de conflito.
Para skills gerenciadas pelo GitHub, o `skill_id` derivado ou explícito, o prefixo do zip de release, o prefixo do checksum, o diretório raiz do zip e o diretório instalado devem ser idênticos.
Os ativos de release usam `{skill_id}-v{version}-skill.zip` e `{skill_id}-v{version}-checksums.txt`; o zip deve conter `{skill_id}/skill.yaml`.

## Integração com o host

- [Runtime architecture](../integration/runtime-model.md)
- [FFI and integration overview](../integration/ffi-overview.md)
- [Host-selected managed runtime roots](../integration/managed-runtime-host-roots.md)
- [Package configuration host contract](../integration/package-configuration.md)
- [Database providers](../integration/database-providers.md)
- [Referências detalhadas em chinês](../zh-CN/index.md#宿主对接)

## SDKs

- [SDK overview](../sdk/index.md)
- [luaskills-sdk-typescript](https://github.com/LuaSkills/luaskills-sdk-typescript) · [luaskills-sdk-python](https://github.com/LuaSkills/luaskills-sdk-python) · [luaskills-sdk-go](https://github.com/LuaSkills/luaskills-sdk-go)

## Exemplos locais

- [Demo em Rust](../../examples/demo-rust/README.md)
- [Demo C FFI](../../examples/ffi/c/README.md)
- [Demo TypeScript FFI](../../examples/ffi/typescript/README.md)
- [Standard Runtime Fixture](../../examples/ffi/standard_runtime/README.md)
- [Host Provider Demo](../../examples/ffi/host_provider_demo/README.md)
