# LuaSkills Dokumentation

[English](../index.md) | [简体中文](../zh-CN/index.md) | [日本語](../ja/index.md) | [한국어](../ko/index.md) | [Español](../es/index.md) | [Français](../fr/index.md) | [Deutsch](index.md) | [Português (BR)](../pt-BR/index.md)

[Deutsches README](../../README.de.md) | [Warum LuaSkills](product/why-luaskills.md)

Die Dokumentation ist nach Aufgabe gegliedert: Skill-Entwicklung, Host-Integration und SDKs. Die Fachdokumente sind auf Englisch; tiefe Host- und FFI-Referenzen werden auf Chinesisch gepflegt.

## Einstieg

| Sie möchten | Einstieg |
| --- | --- |
| LuaSkills bewerten | [Warum LuaSkills](product/why-luaskills.md) |
| Einen Lua-Skill schreiben | [Skill development manual](../development/skill-development.md) |
| Ein System Plugin oder verwaltete Python/Node-Abläufe schreiben | [System Plugin managed runtime guide](../development/system-plugin-managed-runtime.md) |
| LuaSkills in einen Rust-Host einbetten | [Runtime architecture](../integration/runtime-model.md) |
| Über C ABI oder JSON FFI einbetten | [FFI and integration overview](../integration/ffi-overview.md) |
| TypeScript, Python oder Go verwenden | [SDK overview](../sdk/index.md) |

## Skill-Entwicklung

- [Skill development manual](../development/skill-development.md)
- [System Plugin managed runtime guide](../development/system-plugin-managed-runtime.md)
- [demo-skill](https://github.com/LuaSkills/demo-skill): minimale Skill-Vorlage.
- [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit): produktionsreifes Beispiel.

### Skill-Namensregeln

`skill_id` und jedes `entry.name` müssen `^[a-z]([a-z0-9-]*[a-z0-9])?$` erfüllen.
Der physische Skill-Ordnername ist die einzige Quelle für `skill_id`; `skill.yaml` darf kein `skill_id`-Feld deklarieren.
Canonical Entries werden als `{skill_id}-{entry_name}` veröffentlicht und können bei Konflikten ein stabiles `-N`-Suffix erhalten.
Für GitHub-verwaltete Skills müssen abgeleiteter oder expliziter `skill_id`, Release-Zip-Präfix, Checksum-Präfix, oberster Zip-Ordner und Installationsordner identisch sein.
Release-Dateien verwenden `{skill_id}-v{version}-skill.zip` und `{skill_id}-v{version}-checksums.txt`; das Zip muss `{skill_id}/skill.yaml` enthalten.

## Host-Integration

- [Runtime architecture](../integration/runtime-model.md)
- [FFI and integration overview](../integration/ffi-overview.md)
- [Host-selected managed runtime roots](../integration/managed-runtime-host-roots.md)
- [Package configuration host contract](../integration/package-configuration.md)
- [Database providers](../integration/database-providers.md)
- [Ausführliche chinesische Referenzen](../zh-CN/index.md#宿主对接)

## SDKs

- [SDK overview](../sdk/index.md)
- [luaskills-sdk-typescript](https://github.com/LuaSkills/luaskills-sdk-typescript) · [luaskills-sdk-python](https://github.com/LuaSkills/luaskills-sdk-python) · [luaskills-sdk-go](https://github.com/LuaSkills/luaskills-sdk-go)

## Lokale Beispiele

- [Rust-Demo](../../examples/demo-rust/README.md)
- [C FFI Demo](../../examples/ffi/c/README.md)
- [TypeScript FFI Demo](../../examples/ffi/typescript/README.md)
- [Standard Runtime Fixture](../../examples/ffi/standard_runtime/README.md)
- [Host Provider Demo](../../examples/ffi/host_provider_demo/README.md)
