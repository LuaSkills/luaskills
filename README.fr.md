<div align="center">

# LuaSkills

**Un runtime Rust qui transforme des scripts Lua en skills de qualité produit**

Cœur Rust · skills LuaJIT · Rust API, C ABI, JSON FFI et SDK TypeScript / Python / Go

[![crates.io](https://img.shields.io/crates/v/luaskills.svg)](https://crates.io/crates/luaskills)
[![docs.rs](https://img.shields.io/docsrs/luaskills)](https://docs.rs/luaskills)
[![License: MIT](https://img.shields.io/badge/License-MIT-green.svg)](LICENSE)
[![Release](https://img.shields.io/github/v/release/LuaSkills/luaskills)](https://github.com/LuaSkills/luaskills/releases)

[English](README.md) | [简体中文](README.zh-CN.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | **Français** | [Deutsch](README.de.md) | [Português (BR)](README.pt-BR.md)

[Documentation](docs/fr/index.md) · [Modèle de skill](https://github.com/LuaSkills/demo-skill) · [Exemple CodeKit](https://github.com/LuaSkills/vulcan-codekit)

</div>

> Ceci est une traduction du [README anglais](README.md). En cas de divergence, la version anglaise fait foi.

---

## À propos

LuaSkills charge, exécute et gère des paquets de skills Lua au sein d’une application hôte. L’hôte obtient des outils scriptables, une aide structurée, des runtimes Python et Node.js gérés, la gestion des dépendances et une exécution qui tient compte des bases de données, sans construire son propre runtime de plugins.

**LuaSkills exécute les skills ; l’hôte décide comment ils deviennent des fonctionnalités produit.** Politiques, authentification, demandes d’autorisation, budgets, emplacement du stockage et expérience utilisateur restent du côté de l’hôte.

LuaSkills fait tourner le système de skills de [Vulcan Code](https://github.com/OpenVulcan/vulcan-code). Il convient à tout agent IA, IDE ou produit de bureau ou serveur qui a besoin d’un système de skills maîtrisé plutôt que de scripts embarqués isolés.

## Points forts

### 🧩 Runtime de skills

- Découvrir, charger, lister, appeler et recharger des skills dans des racines en couches `ROOT`, `PROJECT` et `USER`
- Arbres d’aide stricts que l’hôte affiche en documentation, descriptions d’outils, palettes de commandes ou panneaux
- Un schéma d’entrée objet complet par entrée, avec des noms canoniques `{skill_id}-{entry_name}`
- Installation, mise à jour et désinstallation depuis des releases GitHub ou des URL, avec dépendances vérifiées par checksum

### 🧰 API Lua standard

- `vulcan.fs`, `vulcan.path`, `vulcan.process`, `vulcan.io`, `vulcan.os`, `vulcan.json`, `vulcan.cache`
- `vulcan.context` pour la requête, le client, le budget et les répertoires du skill ; `vulcan.deps` pour les racines de dépendances
- `vulcan.host.*` appelle les outils enregistrés par l’hôte ; `vulcan.models.*` utilise les modèles d’embedding et LLM gérés par l’hôte
- `vulcan.config.*` fournit à chaque paquet une configuration typée et validée, avec stockages versionnés et événements de changement

### 🐍 Python et Node.js gérés

- Les skills appellent des gestionnaires Python et Node.js versionnés via `vulcan.runtime.python.*` et `vulcan.runtime.node.*`
- Dépendances verrouillées, sortie bornée et nettoyage déterministe de l’arbre de processus
- Sessions persistantes entre appels pour les leases System Plugin stricts, avec événements hôte et callback de réveil sur front
- Racines de distribution en lecture seule et racines d’environnement inscriptibles choisies par l’hôte

### 🔌 Intégration hôte

- API du crate pour les hôtes Rust
- C ABI standard et FFI publique `_json` pour tous les autres langages
- `runtime_lease` public et `system_runtime_lease` lié à une autorité pour des VM Lua persistantes
- Résultats structurés `host_result`, à commencer par les éditions `change_set` de niveau IDE
- SQLite optionnel via des callbacks enregistrés par l’hôte

### 📦 SDK

- SDK [TypeScript / Node.js](https://github.com/LuaSkills/luaskills-sdk-typescript), [Python](https://github.com/LuaSkills/luaskills-sdk-python) et [Go](https://github.com/LuaSkills/luaskills-sdk-go) au-dessus de la FFI JSON
- Installation des ressources du runtime avec vérification SHA-256

## Ce que LuaSkills n’est pas

- Pas un serveur MCP à lui seul ; [vulcan-mcp](https://github.com/OpenVulcan/vulcan-mcp) fournit une couche hôte MCP
- Ni un lecteur de configuration hôte, ni un calculateur de budget, ni un moteur de rendu d’interface
- **Pas un bac à sable.** Les skills sont du code de confiance. L’hôte décide des racines activées, des skills installés, des actions de gestion exposées, des modes de base de données autorisés et de l’autorité portée par chaque opération.

## Architecture

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

| Répertoire | Description |
| --- | --- |
| `src/runtime` | Moteur, VM Lua, leases, contexte, aide, résultats, runtimes gérés |
| `src/skill` | Manifestes, racines, configuration et cycle de vie |
| `src/dependency`, `src/download` | Résolution des dépendances, téléchargements et extraction d’archives |
| `src/providers` | Liaisons SQLite |
| `src/ffi`, `src/ffi_standard`, `src/ffi_embedded` | FFI publique `_json`, C ABI standard et transport du runtime embarqué |
| `include/` | En-têtes C pour les hôtes FFI |
| `contracts/` | Contrats machine partagés avec les SDK |
| `examples/` | Exemples Rust, FFI, runtimes gérés et configuration |

## Démarrage

### Hôtes Rust

```toml
[dependencies]
luaskills = "0.7.4"
```

[`examples/demo-rust`](examples/demo-rust/README.md) montre `call_skill` et le pont d’outils `vulcan.host.*`.

### Autres langages

| Hôte | Voie |
| --- | --- |
| TypeScript / Node.js | `npm install @luaskills/sdk` |
| Python | `pip install luaskills-sdk` |
| Go | `go get github.com/LuaSkills/luaskills-sdk-go` |
| C / C++ | Téléchargez `luaskills-ffi-sdk-{platform}.tar.gz` depuis les [Releases](https://github.com/LuaSkills/luaskills/releases) et utilisez la C ABI standard |

Voir le [SDK overview](docs/sdk/index.md) et le [FFI overview](docs/integration/ffi-overview.md).

### Écrire un skill

Partez de [demo-skill](https://github.com/LuaSkills/demo-skill), puis lisez le [Skill development manual](docs/development/skill-development.md). Pour déboguer un skill avec une arborescence de runtime réelle :

```bash
cargo run --features debug-sqlite --bin luaskills-debug -- call \
  --runtime-root D:/runtime \
  --skill-path D:/skills/demo-skill \
  --tool ping \
  --args-json "{\"note\":\"hello\"}"
```

`luaskills-debug` copie le skill dans `runtime_root/skills/<skill_id>` et suit le chemin normal `load_from_roots -> call_skill`, afin que racines de dépendances, état, bases de données et contexte correspondent à une exécution hôte réelle. C’est un outil de développement du dépôt ; il n’ajoute aucune API SDK ou FFI.

## Documentation

| Thème | Documents |
| --- | --- |
| Développement de skills | [Skill development manual](docs/development/skill-development.md) · [System Plugin managed runtime guide](docs/development/system-plugin-managed-runtime.md) |
| Intégration hôte | [Runtime architecture](docs/integration/runtime-model.md) · [FFI overview](docs/integration/ffi-overview.md) · [Managed runtime roots](docs/integration/managed-runtime-host-roots.md) · [Package configuration](docs/integration/package-configuration.md) · [Database providers](docs/integration/database-providers.md) |
| SDK | [SDK overview](docs/sdk/index.md) |
| Tout | [Documentation en français](docs/fr/index.md) · [Documentation hub](docs/index.md) |

## Écosystème

| Dépôt | Description |
| --- | --- |
| [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit) | Navigation de code, inspection AST, recherche structurelle, navigation Markdown et correctifs sûrs |
| [vulcan-curl](https://github.com/LuaSkills/vulcan-curl) | Requêtes HTTP GET / POST structurées |
| [vulcan-file](https://github.com/LuaSkills/vulcan-file) | Listages respectant les règles d’exclusion, lectures exactes et petites modifications avec aperçu |
| [vulcan-lua](https://github.com/LuaSkills/vulcan-lua) | Exécution Lua bornée, en ligne ou depuis un fichier |
| [vulcan-testkit](https://github.com/LuaSkills/vulcan-testkit) | Transforme la sortie de build, tests, lint et vérification de types en diagnostics compacts |
| [vulcan-workmem](https://github.com/LuaSkills/vulcan-workmem) | Mémoire de travail par projet pour points de contrôle et passations |
| [demo-skill](https://github.com/LuaSkills/demo-skill) | Modèle minimal de dépôt de skill |
| [luaskills-packages](https://github.com/LuaSkills/luaskills-packages) | Paquets du runtime Lua et dépendances natives |

## Versions

| Composant | Version |
| --- | --- |
| Cœur LuaSkills et FFI SDK | `0.7.4` |
| SDK TypeScript, Python et Go | Lignes de version indépendantes ; voir le [SDK overview](docs/sdk/index.md#versions) |
| Paquets du runtime Lua | série compatible `0.1` |
| Python / uv gérés | `3.14.6` / `0.11.28` |
| Node.js / pnpm gérés | `24.18.0` / `11.11.0` |
| Valeurs par défaut du runtime géré | `workers=4 / idle=60s / sessions=256 / buffer=1MiB/stream / invoke=unlimited` |

Les valeurs par défaut des scripts se trouvent dans `scripts/deps/`. Les publications suivent le [processus de release](scripts/release/README.md) : d’abord les paquets du runtime, puis la release du cœur sur cinq plateformes, ensuite le crate, enfin les SDK.

## Contribuer

Issues et pull requests sont les bienvenues.

1. Forkez le dépôt et créez une branche depuis `main`.
2. Vérifiez en local :

   ```bash
   cargo fmt --all -- --check
   cargo clippy --all-targets -- -D warnings
   cargo test --all-targets -- --test-threads=1
   cargo deny check
   ```

3. Signez chaque commit avec `git commit -s` ; voir [CONTRIBUTING.md](CONTRIBUTING.md) pour le DCO.
4. Gardez chaque pull request centrée sur un seul sujet et expliquez ce qui a changé, pourquoi et comment vous l’avez vérifié.

## Licence

[MIT](LICENSE). Les mentions tierces figurent dans [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
