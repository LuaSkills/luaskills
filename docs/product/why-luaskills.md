# Why LuaSkills

[English](why-luaskills.md) | [简体中文](../zh-CN/product/why-luaskills.md) | [日本語](../ja/product/why-luaskills.md) | [한국어](../ko/product/why-luaskills.md) | [Español](../es/product/why-luaskills.md) | [Français](../fr/product/why-luaskills.md) | [Deutsch](../de/product/why-luaskills.md) | [Português (BR)](../pt-BR/product/why-luaskills.md)

[Documentation hub](../index.md)

LuaSkills exists because modern host applications need more than embedded scripting.
They need skills that can be packaged, discovered, documented, called, updated, and integrated into product policy without each host rebuilding the same runtime layer.

## The Product Problem

Most products eventually want user-visible automation:

- Tools for AI agents.
- Developer workflows for IDEs and local assistants.
- Search, memory, and database-backed capabilities.
- First-party skills shipped by the product.
- User or project skills installed later.
- A clean path from prototype scripts to supported extensions.

The hard part is not just running Lua.
The hard part is making Lua skills fit a product boundary:

- What is installed?
- What is callable?
- Which root owns it?
- Which host authority is used?
- Where do dependencies live?
- Who owns the database?
- How does the UI explain what the skill does?

LuaSkills is built around those questions.

## What LuaSkills Gives You

LuaSkills gives hosts a runtime contract instead of a pile of conventions.

It standardizes:

- Skill package loading.
- Entry discovery and invocation.
- Strict help trees.
- Runtime context injection.
- Skill dependency path injection.
- SQLite provider routing.
- Root layering for system, project, and user skills.
- Rust, C ABI, and public `_json` FFI integration.

The result is a runtime that can power command palettes, MCP tools, desktop apps, local agents, service hosts, or internal platforms.

## Capability Categories

| Category | What It Enables |
| --- | --- |
| Runtime core | Load skills, reload roots, list entries, and call skill functions. |
| Skill authoring | Write Lua entries with stable `vulcan.*` APIs and structured help. |
| Product control | Keep policy, permissions, budgets, UI, and authority in the host. |
| Data-aware skills | Route SQLite through host-owned callbacks. |
| Multi-language hosts | Integrate from Rust, C ABI, TypeScript, Python, Go, or mixed systems. |
| Ecosystem growth | Use real examples and templates instead of inventing every package shape. |

## Multi-language Integration

LuaSkills supports several host styles:

- Rust hosts can call the crate directly.
- Low-level hosts can use the standard C ABI.
- Dynamic or SDK-friendly hosts can use the public `_json` FFI.
- TypeScript, Python, and Go users can build through dedicated SDK repositories.

This lets a product team choose the right integration layer without changing the skill model.

## Skill Ecosystem

Two repositories are especially important:

- [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit) shows what a serious LuaSkills package can look like. It exposes code navigation, AST inspection, structural search, Markdown navigation, and patch workflows as a skill product.
- [demo-skill](https://github.com/LuaSkills/demo-skill) shows the minimal repository shape for a skill package.

Together they define the learning path:

1. Learn the package shape from `demo-skill`.
2. Study real product behavior in `vulcan-codekit`.
3. Use LuaSkills as the runtime contract in your own host.

## Trust and Control

LuaSkills does not pretend that arbitrary Lua is magically safe.
The current runtime treats skills as trusted code by default.

That is a product decision, not an accident.
It keeps LuaSkills focused on runtime correctness while leaving security policy to the host:

- Which roots are writable?
- Which skills can be installed?
- Which operations require system authority?
- Which database mode is allowed?
- Which user sees which tools?

## When LuaSkills Is the Right Fit

LuaSkills is a good fit when you need:

- Repeatable skill packaging.
- Runtime-managed help and entry metadata.
- A host-controlled permission and presentation layer.
- Local or embedded tool execution.
- Database-aware skills.
- A path from internal tools to public skill ecosystems.

It is less suitable when you only need a one-off script runner or when you need a hardened sandbox for arbitrary untrusted code today.

## Bottom Line

LuaSkills is a runtime layer for turning Lua packages into product-grade skills.
It keeps skills portable, hosts in control, and integrations open across languages.
