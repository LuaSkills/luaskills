# LuaSkills

[English](README.md) | [简体中文](README.zh-CN.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (BR)](README.pt-BR.md)

[Documentation](docs/fr/index.md) | [Template de skill](https://github.com/LuaSkills/demo-skill) | [Exemple CodeKit](https://github.com/LuaSkills/vulcan-codekit)

LuaSkills est un runtime écrit en Rust pour charger, exécuter et gérer des skills écrits en Lua.
Il permet à une application hôte d'ajouter des outils scriptables, une aide structurée, des APIs de runtime, des chemins de dépendances et une intégration SQLite / LanceDB sans reconstruire un runtime de plugins.

En une phrase : LuaSkills exécute les skills ; l'hôte décide comment les exposer comme fonctionnalités produit.

## Ce Que C'est

LuaSkills est la couche runtime centrale de l'écosystème LuaSkills.
Il est conçu pour les applications qui veulent un système de skills contrôlé plutôt que des scripts isolés.

Il fournit :

- Découverte, chargement, énumération et invocation de skills.
- Strict help trees que l'hôte peut afficher comme documentation, palette de commandes, tools ou UI.
- Injection des APIs standard `vulcan.*` et `vulcan.runtime.*`.
- Sessions Python et Node.js persistantes et isolées par paquet pour les skills et System Plugins sous Windows x64, Linux x64/ARM64 et macOS x64/ARM64 ; Windows ARM est explicitement non pris en charge.
- Contexte de runtime pour la requête courante, les dossiers de skill, les ressources, les dépendances et les métadonnées client.
- Intégration optionnelle SQLite et LanceDB pour des skills avec état ou mémoire.
- API Rust, C ABI standard et FFI publique `_json`.
- Chemins d'intégration SDK pour TypeScript, Python et Go.

## Ce Que Ce N'est Pas

LuaSkills ne possède pas toute la surface produit.

Ce n'est pas :

- Un MCP server autonome.
- Un lecteur de fichiers de configuration hôte.
- Un calculateur de budget client.
- Un moteur de rendu UI produit.
- Une frontière sandbox pour du code Lua non fiable.

L'hôte garde le contrôle des permissions, de l'authentification, de l'UI, des budgets, du stockage et de l'exposition utilisateur.

## Écosystème

- [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit) : exemple LuaSkills important et proche de la production, avec navigation de code, inspection AST, recherche structurée, navigation Markdown et workflows de patch.
- [vulcan-curl](https://github.com/LuaSkills/vulcan-curl) : skill de requêtes HTTP avec entries GET / POST structurées et exécution de requêtes de style curl.
- [vulcan-file](https://github.com/LuaSkills/vulcan-file) : skill d'opérations fichier pour lister avec règles ignore, lire du texte exact et effectuer de petites éditions avec aperçu.
- [vulcan-lua](https://github.com/LuaSkills/vulcan-lua) : skill d'exécution Lua contrôlée pour du code inline ou des tâches basées sur des fichiers Lua.
- [vulcan-testkit](https://github.com/LuaSkills/vulcan-testkit) : routeur de validation qui transforme les sorties build, test, lint et typecheck en diagnostics compacts.
- [vulcan-workmem](https://github.com/LuaSkills/vulcan-workmem) : skill de mémoire de travail par projet pour checkpoints de tâche et contexte de handoff durable.
- [demo-skill](https://github.com/LuaSkills/demo-skill) : template minimal pour apprendre `skill.yaml`, les runtime entries, les fichiers help et la structure d'un dépôt.
- [luaskills-sdk-typescript](https://github.com/LuaSkills/luaskills-sdk-typescript) : SDK TypeScript / Node.js.
- [luaskills-sdk-python](https://github.com/LuaSkills/luaskills-sdk-python) : SDK Python.
- [luaskills-sdk-go](https://github.com/LuaSkills/luaskills-sdk-go) : SDK Go.

## Documentation

- [Entrée française](docs/fr/index.md)
- [Pourquoi LuaSkills](docs/fr/product/why-luaskills.md)
- [Entrée anglaise](docs/index.md)
- [Documentation technique détaillée en chinois](docs/zh-CN/index.md)

## Chemins D'intégration

| Type d'hôte | Chemin recommandé |
| --- | --- |
| Rust | Utiliser directement le crate Rust. |
| C / C++ / hôte bas niveau | Utiliser le C ABI standard. |
| TypeScript / Node.js | Préférer `luaskills-sdk-typescript`. |
| Python | Préférer `luaskills-sdk-python`. |
| Go | Choisir `luaskills-sdk-go` ou le C ABI standard selon les callbacks et le déploiement. |

## Démarrage Rapide

Hôte Rust :

```toml
[dependencies]
luaskills = "0.5"
```

Commandes de développement :

```bash
cargo check
cargo test --lib
```

Pour apprendre la forme d'un skill :

1. [demo-skill](https://github.com/LuaSkills/demo-skill)
2. [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit)
3. [Skill development manual](docs/skill-development.md)

## Règles De Nommage Des Skills

`skill_id` et chaque `entry.name` doivent respecter `^[a-z]([a-z0-9-]*[a-z0-9])?$`.
Le nom physique du dossier du skill est la seule source de `skill_id`; `skill.yaml` ne doit pas déclarer de champ `skill_id`.
Les entrées canonical utilisent `{skill_id}-{entry_name}` et peuvent recevoir un suffixe stable `-N` en cas de conflit.
Pour les skills gérés par GitHub, le `skill_id` dérivé du dépôt ou explicite, le préfixe du zip de release, le préfixe des checksums, le dossier racine du zip et le dossier d'installation doivent être identiques.
Les assets utilisent `{skill_id}-v{version}-skill.zip` et `{skill_id}-v{version}-checksums.txt`; le zip doit contenir `{skill_id}/skill.yaml`.

## Modèle De Confiance

LuaSkills traite actuellement les skills comme du code fiable par défaut.
Il ne fournit pas de promesse sandbox pour des packages Lua arbitraires et non fiables.

L'hôte doit décider des roots, des skills installables, des actions de gestion, du mode database provider et de l'authority de chaque opération.

## Matrice Des Versions Actuelles

| Composant | Version actuelle |
| --- | --- |
| Cœur LuaSkills, SDK FFI et gamme de SDK de langage | `0.5.0` |
| Série compatible des paquets du runtime Lua | `0.1` |
| Python / uv gérés | `3.12.7` / `0.11.17` |
| Node.js / pnpm gérés | `22.11.0` / `9.15.0` |
| vldb-controller | `0.2.1` |
| vldb-sqlite / vldb-lancedb | `0.1.5` / `0.1.5` |

Les valeurs exécutables par défaut sont définies dans `scripts/deps/fetch_managed_runtimes.*`, `scripts/deps/fetch_deps.*` et `scripts/deps/sync_runtime_assets.*`. `dependencies.yaml` doit déclarer ces versions exactes du runtime et du gestionnaire de paquets, sauf si l’hôte installe et déclare volontairement une autre version exacte prise en charge.

## License

MIT
