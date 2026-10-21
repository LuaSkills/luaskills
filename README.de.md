<div align="center">

# LuaSkills

**Eine Rust-Runtime, die Lua-Skripte zu produktreifen Skills macht**

Rust-Kern · LuaJIT-Skills · Rust API, C ABI, JSON FFI und SDKs für TypeScript / Python / Go

[![crates.io](https://img.shields.io/crates/v/luaskills.svg)](https://crates.io/crates/luaskills)
[![docs.rs](https://img.shields.io/docsrs/luaskills)](https://docs.rs/luaskills)
[![License: MIT](https://img.shields.io/badge/License-MIT-green.svg)](LICENSE)
[![Release](https://img.shields.io/github/v/release/LuaSkills/luaskills)](https://github.com/LuaSkills/luaskills/releases)

[English](README.md) | [简体中文](README.zh-CN.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | **Deutsch** | [Português (BR)](README.pt-BR.md)

[Dokumentation](docs/de/index.md) · [Skill-Vorlage](https://github.com/LuaSkills/demo-skill) · [CodeKit-Beispiel](https://github.com/LuaSkills/vulcan-codekit)

</div>

> Dies ist eine Übersetzung des [englischen README](README.md). Bei Abweichungen gilt die englische Fassung.

---

## Über das Projekt

LuaSkills lädt, führt aus und verwaltet Lua-Skill-Pakete innerhalb einer Host-Anwendung. Der Host erhält skriptbare Werkzeuge, strukturierte Hilfe, verwaltete Python- und Node.js-Runtimes, Abhängigkeitsverwaltung und datenbankfähige Ausführung, ohne eine eigene Plugin-Runtime zu bauen.

**LuaSkills führt Skills aus; der Host entscheidet, wie daraus Produktfunktionen werden.** Richtlinien, Authentifizierung, Berechtigungsabfragen, Budgets, Speicherorte und Nutzererlebnis bleiben beim Host.

LuaSkills betreibt das Skill-System von [Vulcan Code](https://github.com/OpenVulcan/vulcan-code). Es passt zu jedem KI-Agenten, jeder IDE und jedem Desktop- oder Serverprodukt, das statt verstreuter eingebetteter Skripte ein kontrolliertes Skill-System braucht.

## Highlights

### 🧩 Skill-Runtime

- Skills in geschichteten `ROOT`-, `PROJECT`- und `USER`-Wurzeln finden, laden, auflisten, aufrufen und neu laden
- Strenge Hilfebäume, die der Host als Dokumentation, Werkzeugbeschreibung, Befehlspalette oder UI-Panel darstellt
- Ein vollständiges Objekt-Eingabeschema pro Entry mit kanonischen Namen `{skill_id}-{entry_name}`
- Installation, Aktualisierung und Deinstallation aus GitHub-Releases oder URLs, mit prüfsummengesicherten Abhängigkeiten

### 🧰 Standard-Lua-APIs

- `vulcan.fs`, `vulcan.path`, `vulcan.process`, `vulcan.io`, `vulcan.os`, `vulcan.json`, `vulcan.cache`
- `vulcan.context` für Anfrage, Client, Budget und Skill-Verzeichnisse; `vulcan.deps` für Abhängigkeitswurzeln
- `vulcan.host.*` ruft vom Host registrierte Werkzeuge auf; `vulcan.models.*` nutzt vom Host verwaltete Embedding- und LLM-Modelle
- `vulcan.config.*` gibt jedem Paket typisierte, validierte Konfiguration mit versionierten Speichern und Änderungsereignissen

### 🐍 Verwaltetes Python und Node.js

- Skills rufen versionierte Python- und Node.js-Handler über `vulcan.runtime.python.*` und `vulcan.runtime.node.*` auf
- Gesperrte Paketabhängigkeiten, begrenzte Ausgabe und deterministisches Aufräumen des Prozessbaums
- Persistente aufrufübergreifende Sitzungen für strikte System-Plugin-Leases, mit Host-Ereignissen und flankengesteuertem Wake-Callback
- Vom Host gewählte schreibgeschützte Distributionswurzeln und beschreibbare Umgebungswurzeln

### 🔌 Host-Integration

- Crate-API für Rust-Hosts
- Standard-C-ABI und öffentliche `_json`-FFI für alle anderen Sprachen
- Öffentliches `runtime_lease` und autoritätsgebundenes `system_runtime_lease` für persistente Lua-VMs
- Strukturierte `host_result`-Rückgaben, beginnend mit `change_set`-Bearbeitungen auf IDE-Niveau
- Optional SQLite und LanceDB über dynamische Bibliotheken, Host-Callbacks oder einen Space Controller

### 📦 SDKs

- SDKs für [TypeScript / Node.js](https://github.com/LuaSkills/luaskills-sdk-typescript), [Python](https://github.com/LuaSkills/luaskills-sdk-python) und [Go](https://github.com/LuaSkills/luaskills-sdk-go) über die JSON-FFI
- Installation der Runtime-Assets mit SHA-256-Prüfung

## Was LuaSkills nicht ist

- Kein eigenständiger MCP-Server; [vulcan-mcp](https://github.com/OpenVulcan/vulcan-mcp) stellt eine MCP-Host-Schicht bereit
- Kein Leser für Host-Konfiguration, kein Budgetrechner und kein UI-Renderer
- **Keine Sandbox.** Skills gelten als vertrauenswürdiger Code. Der Host entscheidet, welche Wurzeln aktiv sind, welche Skills installiert werden, welche Verwaltungsaktionen sichtbar sind, welche Datenbankmodi erlaubt sind und welche Autorität jede Operation trägt.

## Architektur

```
┌──────────────────────────────┐         ┌──────────────────────────────────────┐
│  Host application            │  Rust   │  LuaSkills runtime                   │
│  policy · UI · permissions   │  C ABI  │  skill loader · help · lifecycle     │
│  budgets · model providers   │◀───────▶│  LuaJIT VMs · leases · vulcan.* APIs │
│  host tools · storage        │  JSON   │  managed Python / Node · deps        │
│                              │  FFI    │  config stores · SQLite / LanceDB    │
└──────────────────────────────┘         └──────────────────────────────────────┘
                                                          │
                                              skills/<skill_id>/skill.yaml
```

| Verzeichnis | Beschreibung |
| --- | --- |
| `src/runtime` | Engine, Lua-VMs, Leases, Kontext, Hilfe, Ergebnisse, verwaltete Runtimes |
| `src/skill` | Manifeste, Wurzeln, Konfiguration und Lebenszyklus |
| `src/dependency`, `src/download` | Abhängigkeitsauflösung, Downloads und Archiv-Entpackung |
| `src/providers` | SQLite- und LanceDB-Anbindungen |
| `src/ffi`, `src/ffi_standard`, `src/ffi_embedded` | Öffentliche `_json`-FFI, Standard-C-ABI und Transport der eingebetteten Runtime |
| `include/` | C-Header für FFI-Hosts |
| `contracts/` | Maschinenlesbare Verträge, die mit den SDKs geteilt werden |
| `examples/` | Beispiele für Rust, FFI, verwaltete Runtimes und Konfiguration |

## Erste Schritte

### Rust-Hosts

```toml
[dependencies]
luaskills = "0.6.2"
```

[`examples/demo-rust`](examples/demo-rust/README.md) zeigt `call_skill` und die `vulcan.host.*`-Werkzeugbrücke.

### Andere Sprachen

| Host | Weg |
| --- | --- |
| TypeScript / Node.js | `npm install @luaskills/sdk` |
| Python | `pip install luaskills-sdk` |
| Go | `go get github.com/LuaSkills/luaskills-sdk-go` |
| C / C++ | `luaskills-ffi-sdk-{platform}.tar.gz` aus den [Releases](https://github.com/LuaSkills/luaskills/releases) laden und die Standard-C-ABI nutzen |

Siehe [SDK overview](docs/sdk/index.md) und [FFI overview](docs/integration/ffi-overview.md).

### Einen Skill schreiben

Beginnen Sie mit [demo-skill](https://github.com/LuaSkills/demo-skill) und lesen Sie dann das [Skill development manual](docs/development/skill-development.md). So debuggen Sie einen Skill mit echter Runtime-Struktur:

```bash
cargo run --bin luaskills-debug -- call \
  --runtime-root D:/runtime \
  --skill-path D:/skills/demo-skill \
  --tool ping \
  --args-json "{\"note\":\"hello\"}"
```

`luaskills-debug` kopiert den Skill nach `runtime_root/skills/<skill_id>` und nutzt den normalen Pfad `load_from_roots -> call_skill`, sodass Abhängigkeitswurzeln, Zustand, Datenbanken und Kontext der echten Host-Ausführung entsprechen. Es ist ein Entwicklungswerkzeug des Repositorys und fügt keine SDK- oder FFI-API hinzu.

## Dokumentation

| Thema | Dokumente |
| --- | --- |
| Skill-Entwicklung | [Skill development manual](docs/development/skill-development.md) · [System Plugin managed runtime guide](docs/development/system-plugin-managed-runtime.md) |
| Host-Integration | [Runtime architecture](docs/integration/runtime-model.md) · [FFI overview](docs/integration/ffi-overview.md) · [Managed runtime roots](docs/integration/managed-runtime-host-roots.md) · [Package configuration](docs/integration/package-configuration.md) · [Database providers](docs/integration/database-providers.md) |
| SDKs | [SDK overview](docs/sdk/index.md) |
| Alles | [Deutsche Dokumentation](docs/de/index.md) · [Documentation hub](docs/index.md) |

## Ökosystem

| Repository | Beschreibung |
| --- | --- |
| [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit) | Code-Navigation, AST-Inspektion, strukturelle Suche, Markdown-Navigation und sichere Patches |
| [vulcan-curl](https://github.com/LuaSkills/vulcan-curl) | Strukturierte HTTP-GET- / POST-Anfragen |
| [vulcan-file](https://github.com/LuaSkills/vulcan-file) | Auflistung mit Ignore-Regeln, exaktes Lesen und kleine Änderungen mit Vorschau |
| [vulcan-lua](https://github.com/LuaSkills/vulcan-lua) | Begrenzte Lua-Ausführung inline oder aus Dateien |
| [vulcan-testkit](https://github.com/LuaSkills/vulcan-testkit) | Macht aus Build-, Test-, Lint- und Typprüfungsausgaben kompakte Diagnosen |
| [vulcan-workmem](https://github.com/LuaSkills/vulcan-workmem) | Projektbezogener Arbeitsspeicher für Checkpoints und Übergaben |
| [demo-skill](https://github.com/LuaSkills/demo-skill) | Minimale Skill-Repository-Vorlage |
| [luaskills-packages](https://github.com/LuaSkills/luaskills-packages) | Lua-Runtime-Pakete und native Abhängigkeiten |

## Versionen

| Komponente | Version |
| --- | --- |
| LuaSkills-Kern und FFI SDK | `0.6.2` |
| SDKs für TypeScript, Python und Go | Unabhängige Versionslinien; siehe [SDK overview](docs/sdk/index.md#versions) |
| Lua-Runtime-Pakete | kompatible `0.1`-Serie |
| Verwaltetes Python / uv | `3.14.6` / `0.11.28` |
| Verwaltetes Node.js / pnpm | `24.18.0` / `11.11.0` |
| Standardwerte der verwalteten Runtime | `workers=4 / idle=60s / sessions=256 / buffer=1MiB/stream / invoke=unlimited` |
| vldb-controller | `0.2.3` |
| vldb-sqlite / vldb-lancedb | `0.1.6` / `0.1.5` |

Die Skript-Standardwerte liegen in `scripts/deps/`. Veröffentlichungen folgen dem [Release-Ablauf](scripts/release/README.md): zuerst die Runtime-Pakete, dann das Kern-Release für fünf Plattformen, danach das Crate und zuletzt die SDKs.

## Mitwirken

Issues und Pull Requests sind willkommen.

1. Forken Sie das Repository und erstellen Sie einen Branch von `main`.
2. Lokal prüfen:

   ```bash
   cargo fmt --all -- --check
   cargo clippy --all-targets -- -D warnings
   cargo test --all-targets -- --test-threads=1
   cargo deny check
   ```

3. Signieren Sie jeden Commit mit `git commit -s`; das DCO steht in [CONTRIBUTING.md](CONTRIBUTING.md).
4. Halten Sie jeden Pull Request fokussiert und erklären Sie, was sich geändert hat, warum und wie Sie es geprüft haben.

## Lizenz

[MIT](LICENSE). Hinweise zu Drittanbietern stehen in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
