# Warum LuaSkills

[English](../../product/why-luaskills.md) | [简体中文](../../zh-CN/product/why-luaskills.md) | [日本語](../../ja/product/why-luaskills.md) | [한국어](../../ko/product/why-luaskills.md) | [Español](../../es/product/why-luaskills.md) | [Français](../../fr/product/why-luaskills.md) | [Deutsch](why-luaskills.md) | [Português (BR)](../../pt-BR/product/why-luaskills.md)

[Deutscher Dokumentationseinstieg](../index.md)

LuaSkills löst nicht nur die Frage, wie man Lua ausführt.
Es löst die Frage, wie ein Produkt scripts, tools, Datenbanken, AI-Agent-Workflows und installierbare Erweiterungen mit einer stabilen Runtime-Grenze organisiert.

## Produktproblem

Viele Host-Produkte benötigen früher oder später:

- Lokale tools für AI Agents.
- Workflows für IDEs, Desktop-Apps und Entwicklertools.
- Such-, memory- oder datenbankbasierte Skills.
- Mitgelieferte first-party Skills.
- Project- oder user-level installierbare Skills.

Schwierig ist nicht, Lua zu starten; schwierig ist, Skills sauber in eine Produktgrenze einzupassen.

## Was LuaSkills Liefert

LuaSkills bietet einen Runtime-Vertrag statt einer Sammlung von Konventionen.

- Laden von Skill-Paketen.
- Entry Discovery und Invocation.
- Strict help trees.
- Runtime-Kontext-Injection.
- Dependency-Pfad-Injection.
- SQLite provider routing.
- System-, Project- und User-root layering.
- Rust-, C ABI- und public `_json` FFI-Integration.

## Fähigkeitskategorien

| Kategorie | Was sie ermöglicht |
| --- | --- |
| Runtime Core | Skills laden, roots neu laden, entries listen und skills aufrufen. |
| Skill Authoring | Skills mit `vulcan.*` APIs und strukturierter Hilfe schreiben. |
| Product Control | Berechtigungen, budget, UI und authority im Host behalten. |
| Data-aware Skills | SQLite über runtime, host provider oder space controller verbinden. |
| Multi-language Hosts | Dasselbe Modell aus Rust, C ABI, TypeScript, Python und Go nutzen. |

## Wichtige Beispiele

- [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit): product-grade LuaSkills-Beispiel.
- [demo-skill](https://github.com/LuaSkills/demo-skill): minimales Skill-Repository-Template.

## Fazit

LuaSkills ist eine Runtime-Schicht, die Lua-Pakete in produktfähige Skills verwandelt.
Sie macht Skills portabel, lässt Kontrolle beim Host und bietet einen gemeinsamen Weg für mehrsprachige Integrationen.
