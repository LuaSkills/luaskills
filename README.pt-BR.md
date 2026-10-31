<div align="center">

# LuaSkills

**Um runtime em Rust que transforma scripts Lua em skills de qualidade de produto**

Núcleo em Rust · skills em LuaJIT · Rust API, C ABI, JSON FFI e SDKs de TypeScript / Python / Go

[![crates.io](https://img.shields.io/crates/v/luaskills.svg)](https://crates.io/crates/luaskills)
[![docs.rs](https://img.shields.io/docsrs/luaskills)](https://docs.rs/luaskills)
[![License: MIT](https://img.shields.io/badge/License-MIT-green.svg)](LICENSE)
[![Release](https://img.shields.io/github/v/release/LuaSkills/luaskills)](https://github.com/LuaSkills/luaskills/releases)

[English](README.md) | [简体中文](README.zh-CN.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | **Português (BR)**

[Documentação](docs/pt-BR/index.md) · [Modelo de skill](https://github.com/LuaSkills/demo-skill) · [Exemplo CodeKit](https://github.com/LuaSkills/vulcan-codekit)

</div>

> Esta é uma tradução do [README em inglês](README.md). Em caso de divergência, vale a versão em inglês.

---

## Sobre o projeto

O LuaSkills carrega, executa e gerencia pacotes de skills em Lua dentro de uma aplicação host. O host ganha ferramentas programáveis, ajuda estruturada, runtimes gerenciados de Python e Node.js, gerenciamento de dependências e execução com acesso a bancos de dados, sem construir seu próprio runtime de plugins.

**O LuaSkills executa skills; o host decide como elas viram funcionalidades do produto.** Políticas, autenticação, pedidos de permissão, orçamentos, local de armazenamento e experiência do usuário continuam com o host.

O LuaSkills é o runtime do sistema de skills do [Vulcan Code](https://github.com/OpenVulcan/vulcan-code). Ele serve para qualquer agente de IA, IDE ou produto desktop ou servidor que precise de um sistema de skills controlado em vez de scripts embutidos avulsos.

## Destaques

### 🧩 Runtime de skills

- Descobrir, carregar, listar, chamar e recarregar skills em raízes em camadas `ROOT`, `PROJECT` e `USER`
- Árvores de ajuda estritas que o host exibe como documentação, descrições de ferramentas, paletas de comandos ou painéis
- Um esquema de entrada de objeto completo por entrada, com nomes canônicos `{skill_id}-{entry_name}`
- Instalar, atualizar e desinstalar a partir de releases do GitHub ou URLs, com dependências verificadas por checksum

### 🧰 APIs padrão de Lua

- `vulcan.fs`, `vulcan.path`, `vulcan.process`, `vulcan.io`, `vulcan.os`, `vulcan.json`, `vulcan.cache`
- `vulcan.context` com dados de requisição, cliente, orçamento e diretórios da skill; `vulcan.deps` para as raízes de dependências
- `vulcan.host.*` chama ferramentas registradas pelo host; `vulcan.models.*` usa modelos de embedding e LLM gerenciados pelo host
- `vulcan.config.*` dá a cada pacote configuração tipada e validada, com armazenamentos versionados e eventos de mudança

### 🐍 Python e Node.js gerenciados

- Skills chamam handlers versionados de Python e Node.js via `vulcan.runtime.python.*` e `vulcan.runtime.node.*`
- Dependências travadas, saída limitada e limpeza determinística da árvore de processos
- Sessões persistentes entre chamadas para leases estritos de System Plugin, com eventos do host e callback de despertar por borda
- Raízes de distribuição somente leitura e raízes de ambiente graváveis escolhidas pelo host

### 🔌 Integração com o host

- API do crate para hosts Rust
- C ABI padrão e FFI pública `_json` para qualquer outra linguagem
- `runtime_lease` público e `system_runtime_lease` vinculado a autoridade para VMs Lua persistentes
- Retornos estruturados `host_result`, começando pelas edições `change_set` de nível IDE
- SQLite opcional por callbacks registrados pelo host

### 📦 SDKs

- SDKs de [TypeScript / Node.js](https://github.com/LuaSkills/luaskills-sdk-typescript), [Python](https://github.com/LuaSkills/luaskills-sdk-python) e [Go](https://github.com/LuaSkills/luaskills-sdk-go) sobre a FFI JSON
- Instalação dos recursos do runtime com verificação SHA-256

## O que o LuaSkills não é

- Não é um servidor MCP por si só; o [vulcan-mcp](https://github.com/OpenVulcan/vulcan-mcp) fornece uma camada de host MCP
- Não lê a configuração do host, não calcula orçamentos e não renderiza interfaces
- **Não é um sandbox.** Skills são código confiável. O host decide quais raízes ficam ativas, quais skills são instaladas, quais ações de gerenciamento são expostas, quais modos de banco de dados são permitidos e qual autoridade cada operação carrega.

## Arquitetura

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

| Diretório | Descrição |
| --- | --- |
| `src/runtime` | Motor, VMs Lua, leases, contexto, ajuda, resultados e runtimes gerenciados |
| `src/skill` | Manifestos, raízes, configuração e ciclo de vida |
| `src/dependency`, `src/download` | Resolução de dependências, downloads e extração de arquivos |
| `src/providers` | Integrações SQLite |
| `src/ffi`, `src/ffi_standard`, `src/ffi_embedded` | FFI pública `_json`, C ABI padrão e transporte do runtime embarcado |
| `include/` | Cabeçalhos C para hosts FFI |
| `contracts/` | Contratos de máquina compartilhados com os SDKs |
| `examples/` | Exemplos de Rust, FFI, runtimes gerenciados e configuração |

## Primeiros passos

### Hosts Rust

```toml
[dependencies]
luaskills = "0.7.4"
```

[`examples/demo-rust`](examples/demo-rust/README.md) mostra `call_skill` e a ponte de ferramentas `vulcan.host.*`.

### Outras linguagens

| Host | Caminho |
| --- | --- |
| TypeScript / Node.js | `npm install @luaskills/sdk` |
| Python | `pip install luaskills-sdk` |
| Go | `go get github.com/LuaSkills/luaskills-sdk-go` |
| C / C++ | Baixe `luaskills-ffi-sdk-{platform}.tar.gz` em [Releases](https://github.com/LuaSkills/luaskills/releases) e use a C ABI padrão |

Veja o [SDK overview](docs/sdk/index.md) e o [FFI overview](docs/integration/ffi-overview.md).

### Escrevendo uma skill

Comece pelo [demo-skill](https://github.com/LuaSkills/demo-skill) e leia o [Skill development manual](docs/development/skill-development.md). Para depurar uma skill com uma estrutura de runtime real:

```bash
cargo run --features debug-sqlite --bin luaskills-debug -- call \
  --runtime-root D:/runtime \
  --skill-path D:/skills/demo-skill \
  --tool ping \
  --args-json "{\"note\":\"hello\"}"
```

O `luaskills-debug` copia a skill para `runtime_root/skills/<skill_id>` e segue o caminho normal `load_from_roots -> call_skill`, de modo que raízes de dependências, estado, bancos de dados e contexto correspondem à execução real do host. É uma ferramenta de desenvolvimento do repositório e não adiciona API ao SDK nem à FFI.

## Documentação

| Tema | Documentos |
| --- | --- |
| Desenvolvimento de skills | [Skill development manual](docs/development/skill-development.md) · [System Plugin managed runtime guide](docs/development/system-plugin-managed-runtime.md) |
| Integração com o host | [Runtime architecture](docs/integration/runtime-model.md) · [FFI overview](docs/integration/ffi-overview.md) · [Managed runtime roots](docs/integration/managed-runtime-host-roots.md) · [Package configuration](docs/integration/package-configuration.md) · [Database providers](docs/integration/database-providers.md) |
| SDKs | [SDK overview](docs/sdk/index.md) |
| Tudo | [Documentação em português](docs/pt-BR/index.md) · [Documentation hub](docs/index.md) |

## Ecossistema

| Repositório | Descrição |
| --- | --- |
| [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit) | Navegação de código, inspeção de AST, busca estrutural, navegação em Markdown e patches seguros |
| [vulcan-curl](https://github.com/LuaSkills/vulcan-curl) | Requisições HTTP GET / POST estruturadas |
| [vulcan-file](https://github.com/LuaSkills/vulcan-file) | Listagem que respeita regras de ignore, leituras exatas e pequenas edições com pré-visualização |
| [vulcan-lua](https://github.com/LuaSkills/vulcan-lua) | Execução limitada de Lua inline ou a partir de arquivos |
| [vulcan-testkit](https://github.com/LuaSkills/vulcan-testkit) | Transforma saídas de build, testes, lint e checagem de tipos em diagnósticos compactos |
| [vulcan-workmem](https://github.com/LuaSkills/vulcan-workmem) | Memória de trabalho por projeto para checkpoints e passagens de contexto |
| [demo-skill](https://github.com/LuaSkills/demo-skill) | Modelo mínimo de repositório de skill |
| [luaskills-packages](https://github.com/LuaSkills/luaskills-packages) | Pacotes do runtime Lua e dependências nativas |

## Versões

| Componente | Versão |
| --- | --- |
| Núcleo do LuaSkills e FFI SDK | `0.7.4` |
| SDKs de TypeScript, Python e Go | Linhas de versão independentes; veja o [SDK overview](docs/sdk/index.md#versions) |
| Pacotes do runtime Lua | série compatível `0.1` |
| Python / uv gerenciados | `3.14.6` / `0.11.28` |
| Node.js / pnpm gerenciados | `24.18.0` / `11.11.0` |
| Padrões do runtime gerenciado | `workers=4 / idle=60s / sessions=256 / buffer=1MiB/stream / invoke=unlimited` |

Os padrões dos scripts ficam em `scripts/deps/`. As publicações seguem o [fluxo de release](scripts/release/README.md): primeiro os pacotes do runtime, depois o release do núcleo em cinco plataformas, em seguida o crate e por fim os SDKs.

## Contribuindo

Issues e pull requests são bem-vindos.

1. Faça um fork do repositório e crie um branch a partir de `main`.
2. Verifique localmente:

   ```bash
   cargo fmt --all -- --check
   cargo clippy --all-targets -- -D warnings
   cargo test --all-targets -- --test-threads=1
   cargo deny check
   ```

3. Assine cada commit com `git commit -s`; veja o DCO em [CONTRIBUTING.md](CONTRIBUTING.md).
4. Mantenha cada pull request focado em um único assunto e explique o que mudou, por quê e como você verificou.

## Licença

[MIT](LICENSE). Os avisos de terceiros estão em [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
