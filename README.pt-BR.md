# LuaSkills

[English](README.md) | [简体中文](README.zh-CN.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (BR)](README.pt-BR.md)

[Documentação](docs/pt-BR/index.md) | [Template de skill](https://github.com/LuaSkills/demo-skill) | [Exemplo CodeKit](https://github.com/LuaSkills/vulcan-codekit)

LuaSkills é um runtime em Rust para carregar, executar e gerenciar skills baseadas em Lua.
Ele permite que aplicações host adicionem ferramentas scriptáveis, help estruturado, APIs de runtime, caminhos de dependências e integração com SQLite / LanceDB sem reconstruir uma runtime de plugins.

Em uma frase: LuaSkills executa skills; o host decide como transformá-las em funcionalidades de produto.

## O Que É

LuaSkills é a camada central de runtime do ecossistema LuaSkills.
Ele foi criado para aplicações que precisam de um sistema de skills controlado, não apenas scripts soltos.

Ele fornece:

- Descoberta, carregamento, listagem e chamada de skills.
- Strict help trees que o host pode renderizar como documentação, comandos, tools ou UI.
- Injeção de APIs padrão `vulcan.*` e `vulcan.runtime.*`.
- Sessões persistentes de Python e Node.js isoladas por pacote para skills e System Plugins no Windows x64, Linux x64/ARM64 e macOS x64/ARM64; Windows ARM não é explicitamente suportado.
- Contexto de runtime para request atual, diretórios de skill, recursos, dependências e metadados do cliente.
- Integração opcional com SQLite e LanceDB para skills com estado ou memória.
- API Rust, C ABI padrão e FFI pública `_json`.
- Caminhos de integração SDK para TypeScript, Python e Go.

## O Que Não É

LuaSkills não controla toda a superfície do produto.

Não é:

- Um MCP server por si só.
- Um leitor de configuração do host.
- Uma calculadora de budget do cliente.
- Um renderizador de UI de produto.
- Uma fronteira sandbox para código Lua não confiável.

Permissões, autenticação, UI, budgets, armazenamento e exposição ao usuário continuam sob controle do host.

## Ecossistema

- [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit): exemplo importante e próximo de produção, com navegação de código, inspeção AST, busca estrutural, navegação Markdown e workflows de patch.
- [vulcan-curl](https://github.com/LuaSkills/vulcan-curl): skill de requests HTTP com entradas GET / POST estruturadas e execução de requests no estilo curl.
- [vulcan-file](https://github.com/LuaSkills/vulcan-file): skill de operações de arquivo para listagem com regras de ignore, leitura exata de texto e pequenas edições com prévia.
- [vulcan-lua](https://github.com/LuaSkills/vulcan-lua): skill de execução Lua controlada para código inline ou tarefas baseadas em arquivos Lua.
- [vulcan-testkit](https://github.com/LuaSkills/vulcan-testkit): roteador de validação que transforma saídas de build, test, lint e typecheck em diagnósticos compactos.
- [vulcan-workmem](https://github.com/LuaSkills/vulcan-workmem): skill de memória de trabalho por projeto para checkpoints de tarefa e contexto de handoff persistente.
- [demo-skill](https://github.com/LuaSkills/demo-skill): template mínimo para aprender `skill.yaml`, runtime entries, arquivos help e layout de repositório.
- [luaskills-sdk-typescript](https://github.com/LuaSkills/luaskills-sdk-typescript): SDK para TypeScript / Node.js.
- [luaskills-sdk-python](https://github.com/LuaSkills/luaskills-sdk-python): SDK para Python.
- [luaskills-sdk-go](https://github.com/LuaSkills/luaskills-sdk-go): SDK para Go.

## Documentação

- [Entrada em português do Brasil](docs/pt-BR/index.md)
- [Por Que LuaSkills](docs/pt-BR/product/why-luaskills.md)
- [Entrada em inglês](docs/index.md)
- [Documentação técnica detalhada em chinês](docs/zh-CN/index.md)

## Caminhos De Integração

| Tipo de host | Caminho recomendado |
| --- | --- |
| Rust | Usar diretamente o crate Rust. |
| C / C++ / host de baixo nível | Usar o C ABI padrão. |
| TypeScript / Node.js | Preferir `luaskills-sdk-typescript`. |
| Python | Preferir `luaskills-sdk-python`. |
| Go | Escolher `luaskills-sdk-go` ou C ABI padrão conforme callbacks e implantação. |

## Início Rápido

Host Rust:

```toml
[dependencies]
luaskills = "0.5"
```

Comandos de desenvolvimento:

```bash
cargo check
cargo test --lib
```

Para aprender a forma de uma skill:

1. [demo-skill](https://github.com/LuaSkills/demo-skill)
2. [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit)
3. [Skill development manual](docs/skill-development.md)

## Regras De Nomenclatura De Skills

`skill_id` e cada `entry.name` devem cumprir `^[a-z]([a-z0-9-]*[a-z0-9])?$`.
O nome físico do diretório do skill é a única fonte de `skill_id`; `skill.yaml` não deve declarar um campo `skill_id`.
As entradas canonical usam `{skill_id}-{entry_name}` e podem receber um sufixo estável `-N` em caso de conflito.
Para skills gerenciados pelo GitHub, o `skill_id` derivado do repositório ou explícito, o prefixo do zip de release, o prefixo de checksums, o diretório superior do zip e o diretório instalado devem ser idênticos.
Os assets usam `{skill_id}-v{version}-skill.zip` e `{skill_id}-v{version}-checksums.txt`; o zip deve conter `{skill_id}/skill.yaml`.

## Modelo De Confiança

Atualmente LuaSkills trata skills como código confiável por padrão.
Ele não oferece uma promessa de sandbox para pacotes Lua arbitrários e não confiáveis.

O host deve decidir roots, skills instaláveis, ações de gerenciamento, modo de database provider e authority de cada operação.

## Matriz De Versões Atual

| Componente | Versão atual |
| --- | --- |
| Núcleo LuaSkills, SDK FFI e linha dos SDKs de linguagem | `0.5.2` |
| Série compatível dos pacotes do runtime Lua | `0.1` |
| Python / uv gerenciados | `3.14.6` / `0.11.28` |
| Node.js / pnpm gerenciados | `24.18.0` / `11.11.0` |
| Padrões do motor gerenciado | `workers=4 / ociosidade=60s / sessões=256 / buffer=1MiB/fluxo / invoke=ilimitado` |
| vldb-controller | `0.2.1` |
| vldb-sqlite / vldb-lancedb | `0.1.5` / `0.1.5` |

Os valores executáveis padrão são definidos em `scripts/deps/fetch_managed_runtimes.*`, `scripts/deps/fetch_deps.*` e `scripts/deps/sync_runtime_assets.*`. O `dependencies.yaml` deve declarar essas versões exatas do runtime e do gerenciador de pacotes, a menos que o host instale e declare deliberadamente outra versão exata compatível.

## License

MIT
