# Por Que LuaSkills

[English](../../product/why-luaskills.md) | [简体中文](../../zh-CN/product/why-luaskills.md) | [日本語](../../ja/product/why-luaskills.md) | [한국어](../../ko/product/why-luaskills.md) | [Español](../../es/product/why-luaskills.md) | [Français](../../fr/product/why-luaskills.md) | [Deutsch](../../de/product/why-luaskills.md) | [Português (BR)](why-luaskills.md)

[Entrada em português do Brasil](../index.md)

LuaSkills não resolve apenas como executar Lua.
Ele resolve como um produto pode organizar scripts, tools, bancos de dados, workflows de AI Agent e extensões instaláveis com uma fronteira de runtime estável.

## Problema De Produto

Muitos produtos host acabam precisando de:

- Tools locais para AI Agents.
- Workflows para IDEs, aplicativos desktop e ferramentas de desenvolvimento.
- Skills de busca, memória ou banco de dados.
- Skills first-party entregues com o produto.
- Skills instaláveis em nível de project ou user.

A parte difícil não é iniciar Lua; é fazer as skills caberem em uma fronteira de produto.

## O Que LuaSkills Entrega

LuaSkills fornece um contrato de runtime, não apenas uma coleção de convenções.

- Carregamento de pacotes skill.
- Descoberta e chamada de entries.
- Strict help trees.
- Injeção de contexto de runtime.
- Injeção de caminhos de dependência.
- Routing de providers SQLite.
- Camadas root de system, project e user.
- Integração Rust, C ABI e public `_json` FFI.

## Categorias De Capacidade

| Categoria | O que habilita |
| --- | --- |
| Runtime Core | Carregar skills, recarregar roots, listar entries e chamar skills. |
| Skill Authoring | Escrever skills com APIs `vulcan.*` e help estruturado. |
| Product Control | Manter permissões, budget, UI e authority no host. |
| Data-aware Skills | Conectar SQLite via runtime, host provider ou space controller. |
| Multi-language Hosts | Usar o mesmo modelo a partir de Rust, C ABI, TypeScript, Python e Go. |

## Exemplos Importantes

- [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit): exemplo product-grade de LuaSkills.
- [demo-skill](https://github.com/LuaSkills/demo-skill): template mínimo de repositório skill.

## Conclusão

LuaSkills é uma camada de runtime para transformar pacotes Lua em skills de produto.
Ele torna skills portáteis, mantém o controle no host e oferece uma rota comum para integrações multilíngues.
