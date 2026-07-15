# LuaSkills

[English](README.md) | [简体中文](README.zh-CN.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (BR)](README.pt-BR.md)

[Dokumentation](docs/de/index.md) | [Skill-Template](https://github.com/LuaSkills/demo-skill) | [CodeKit-Beispiel](https://github.com/LuaSkills/vulcan-codekit)

LuaSkills ist eine Rust-basierte Runtime zum Laden, Ausführen und Verwalten von Lua-basierten Skills.
Host-Anwendungen können scriptbare Tools, strukturierte Hilfe, Runtime-APIs, Dependency-Pfade und SQLite / LanceDB-Integration hinzufügen, ohne jedes Mal eine eigene Plugin-Runtime zu bauen.

Kurz gesagt: LuaSkills führt Skills aus; der Host entscheidet, wie daraus Produktfunktionen werden.

## Was Es Ist

LuaSkills ist die zentrale Runtime-Schicht des LuaSkills-Ökosystems.
Es ist für Anwendungen gedacht, die ein kontrolliertes Skill-System statt einzelner Skripte benötigen.

Es bietet:

- Skill-Erkennung, Laden, Entry-Auflistung und Aufruf.
- Strict help trees, die der Host als Dokumentation, Command Palette, Tools oder UI rendern kann.
- Standardisierte Lua-APIs unter `vulcan.*` und `vulcan.runtime.*`.
- Persistente, paketisolierte Python- und Node.js-Sitzungen für Skills und System Plugins auf Windows x64, Linux x64/ARM64 und macOS x64/ARM64; Windows ARM wird ausdrücklich nicht unterstützt.
- Runtime-Kontext für aktuelle Requests, Skill-Verzeichnisse, Ressourcen, Dependencies und Client-Metadaten.
- Optionale SQLite- und LanceDB-Bindings für zustandsbehaftete oder memory-orientierte Skills.
- Rust API, Standard C ABI und öffentliche `_json` FFI.
- SDK-Integrationspfade für TypeScript, Python und Go.

## Was Es Nicht Ist

LuaSkills besitzt nicht die gesamte Produktoberfläche.

Es ist kein:

- eigenständiger MCP server.
- Leser für Host-Konfigurationsdateien.
- Client-Budget-Rechner.
- Produkt-UI-Renderer.
- Sandbox-Grenze für nicht vertrauenswürdigen Lua-Code.

Berechtigungen, Authentifizierung, UI, Budgets, Speicherorte und Benutzeroberflächen bleiben Aufgabe des Hosts.

## Ökosystem

- [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit): wichtiges produktnahes LuaSkills-Beispiel mit Code-Navigation, AST-Inspektion, struktureller Suche, Markdown-Navigation und Patch-Workflows.
- [vulcan-curl](https://github.com/LuaSkills/vulcan-curl): HTTP-Request-Skill mit strukturierten GET- / POST-Entries und curl-artiger Request-Ausführung.
- [vulcan-file](https://github.com/LuaSkills/vulcan-file): File-Operation-Skill für ignore-bewusste Dateilisten, exakte Textlesevorgänge und kleine Preview-first-Edits.
- [vulcan-lua](https://github.com/LuaSkills/vulcan-lua): kontrollierter Lua-Ausführungs-Skill für Inline-Code oder dateibasierte Lua-Aufgaben.
- [vulcan-testkit](https://github.com/LuaSkills/vulcan-testkit): Validation-Router, der Build-, Test-, Lint- und Typecheck-Ausgaben in kompakte Diagnosen überführt.
- [vulcan-workmem](https://github.com/LuaSkills/vulcan-workmem): projektbezogener Working-Memory-Skill für dauerhafte Task-Checkpoints und Handoff-Kontext.
- [demo-skill](https://github.com/LuaSkills/demo-skill): minimales Template zum Lernen von `skill.yaml`, Runtime Entries, Help-Dateien und Repository-Layout.
- [luaskills-sdk-typescript](https://github.com/LuaSkills/luaskills-sdk-typescript): TypeScript / Node.js SDK.
- [luaskills-sdk-python](https://github.com/LuaSkills/luaskills-sdk-python): Python SDK.
- [luaskills-sdk-go](https://github.com/LuaSkills/luaskills-sdk-go): Go SDK.

## Dokumentation

- [Deutscher Dokumentationseinstieg](docs/de/index.md)
- [Warum LuaSkills](docs/de/product/why-luaskills.md)
- [Englischer Dokumentationseinstieg](docs/index.md)
- [Ausführliche chinesische technische Dokumentation](docs/zh-CN/index.md)

## Integrationspfade

| Host-Typ | Empfohlener Pfad |
| --- | --- |
| Rust | Rust crate direkt verwenden. |
| C / C++ / Low-Level-Host | Standard C ABI verwenden. |
| TypeScript / Node.js | `luaskills-sdk-typescript` bevorzugen. |
| Python | `luaskills-sdk-python` bevorzugen. |
| Go | Je nach Callback- und Deployment-Anforderungen `luaskills-sdk-go` oder Standard C ABI wählen. |

## Schnellstart

Rust-Host:

```toml
[dependencies]
luaskills = "0.5"
```

Entwicklungsbefehle:

```bash
cargo check
cargo test --lib
```

Um die Skill-Struktur zu lernen:

1. [demo-skill](https://github.com/LuaSkills/demo-skill)
2. [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit)
3. [Skill development manual](docs/skill-development.md)

## Skill-Namensregeln

`skill_id` und jedes `entry.name` müssen `^[a-z]([a-z0-9-]*[a-z0-9])?$` erfüllen.
Der physische Skill-Ordnername ist die einzige Quelle für `skill_id`; `skill.yaml` darf kein `skill_id`-Feld deklarieren.
Canonical Entries verwenden `{skill_id}-{entry_name}` und können bei Konflikten ein stabiles `-N`-Suffix erhalten.
Für GitHub-verwaltete Skills müssen der aus dem Repository abgeleitete oder explizite `skill_id`, Release-Zip-Präfix, Checksum-Präfix, oberster Zip-Ordner und Installationsordner identisch sein.
Release-Dateien verwenden `{skill_id}-v{version}-skill.zip` und `{skill_id}-v{version}-checksums.txt`; das Zip muss `{skill_id}/skill.yaml` enthalten.

## Vertrauensmodell

LuaSkills behandelt Skills derzeit standardmäßig als vertrauenswürdigen Code.
Es bietet keine Sandbox-Sicherheitszusage für beliebige nicht vertrauenswürdige Lua-Pakete.

Der Host muss roots, installierbare Skills, Verwaltungsaktionen, database provider mode und operation authority festlegen.

## Aktuelle Versionsmatrix

| Komponente | Aktuelle Version |
| --- | --- |
| LuaSkills Core, FFI SDK und Sprach-SDK-Linie | `0.5.3` |
| Kompatible Serie der Lua-Runtime-Pakete | `0.1` |
| Verwaltetes Python / uv | `3.14.6` / `0.11.28` |
| Verwaltetes Node.js / pnpm | `24.18.0` / `11.11.0` |
| Standardwerte der Managed-Runtime-Engine | `Worker=4 / Leerlauf=60s / Sessions=256 / Puffer=1MiB/Stream / invoke=unbegrenzt` |
| vldb-controller | `0.2.1` |
| vldb-sqlite / vldb-lancedb | `0.1.5` / `0.1.5` |

Die ausführbaren Standardwerte werden in `scripts/deps/fetch_managed_runtimes.*`, `scripts/deps/fetch_deps.*` und `scripts/deps/sync_runtime_assets.*` definiert. `dependencies.yaml` muss diese exakten Runtime- und Paketmanager-Versionen deklarieren, sofern der Host nicht bewusst eine andere exakt unterstützte Version installiert und deklariert.

## License

MIT
