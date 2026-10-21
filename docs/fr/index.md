# Documentation LuaSkills

[English](../index.md) | [简体中文](../zh-CN/index.md) | [日本語](../ja/index.md) | [한국어](../ko/index.md) | [Español](../es/index.md) | [Français](index.md) | [Deutsch](../de/index.md) | [Português (BR)](../pt-BR/index.md)

[README en français](../../README.fr.md) | [Pourquoi LuaSkills](product/why-luaskills.md)

La documentation est organisée par tâche : développement de skills, intégration hôte et SDK. Les documents techniques sont en anglais ; les références approfondies sur l’hôte et la FFI sont maintenues en chinois.

## Par où commencer

| Vous voulez | Commencez ici |
| --- | --- |
| Évaluer LuaSkills | [Pourquoi LuaSkills](product/why-luaskills.md) |
| Écrire un skill Lua | [Skill development manual](../development/skill-development.md) |
| Écrire un System Plugin ou des flux Python/Node gérés | [System Plugin managed runtime guide](../development/system-plugin-managed-runtime.md) |
| Intégrer LuaSkills dans un hôte Rust | [Runtime architecture](../integration/runtime-model.md) |
| Intégrer via la C ABI ou la JSON FFI | [FFI and integration overview](../integration/ffi-overview.md) |
| Utiliser TypeScript, Python ou Go | [SDK overview](../sdk/index.md) |

## Développement de skills

- [Skill development manual](../development/skill-development.md)
- [System Plugin managed runtime guide](../development/system-plugin-managed-runtime.md)
- [demo-skill](https://github.com/LuaSkills/demo-skill) : modèle minimal de skill.
- [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit) : exemple de niveau production.

### Règles de nommage des skills

`skill_id` et chaque `entry.name` doivent respecter `^[a-z]([a-z0-9-]*[a-z0-9])?$`.
Le nom physique du répertoire du skill est la seule source de `skill_id` ; `skill.yaml` ne doit pas déclarer de champ `skill_id`.
Les entrées canoniques sont publiées sous la forme `{skill_id}-{entry_name}` et peuvent recevoir un suffixe stable `-N` en cas de conflit.
Pour les skills gérés depuis GitHub, le `skill_id` dérivé ou explicite, le préfixe du zip de release, le préfixe du checksum, le répertoire racine du zip et le répertoire installé doivent être identiques.
Les ressources de release utilisent `{skill_id}-v{version}-skill.zip` et `{skill_id}-v{version}-checksums.txt` ; le zip doit contenir `{skill_id}/skill.yaml`.

## Intégration hôte

- [Runtime architecture](../integration/runtime-model.md)
- [FFI and integration overview](../integration/ffi-overview.md)
- [Host-selected managed runtime roots](../integration/managed-runtime-host-roots.md)
- [Package configuration host contract](../integration/package-configuration.md)
- [Database providers](../integration/database-providers.md)
- [Références détaillées en chinois](../zh-CN/index.md#宿主对接)

## SDK

- [SDK overview](../sdk/index.md)
- [luaskills-sdk-typescript](https://github.com/LuaSkills/luaskills-sdk-typescript) · [luaskills-sdk-python](https://github.com/LuaSkills/luaskills-sdk-python) · [luaskills-sdk-go](https://github.com/LuaSkills/luaskills-sdk-go)

## Exemples locaux

- [Démo Rust](../../examples/demo-rust/README.md)
- [Démo C FFI](../../examples/ffi/c/README.md)
- [Démo TypeScript FFI](../../examples/ffi/typescript/README.md)
- [Standard Runtime Fixture](../../examples/ffi/standard_runtime/README.md)
- [Host Provider Demo](../../examples/ffi/host_provider_demo/README.md)
