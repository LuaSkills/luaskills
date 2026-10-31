# Pourquoi LuaSkills

[English](../../product/why-luaskills.md) | [简体中文](../../zh-CN/product/why-luaskills.md) | [日本語](../../ja/product/why-luaskills.md) | [한국어](../../ko/product/why-luaskills.md) | [Español](../../es/product/why-luaskills.md) | [Français](why-luaskills.md) | [Deutsch](../../de/product/why-luaskills.md) | [Português (BR)](../../pt-BR/product/why-luaskills.md)

[Entrée française](../index.md)

LuaSkills ne résout pas seulement l'exécution de Lua.
Il résout la manière dont un produit peut organiser scripts, tools, bases de données, workflows AI Agent et extensions installables avec une frontière runtime stable.

## Problème Produit

Beaucoup de produits hôtes finissent par avoir besoin de :

- Tools locaux pour AI Agents.
- Workflows pour IDEs, applications desktop et outils développeur.
- Skills de recherche, mémoire ou base de données.
- Skills first-party livrés avec le produit.
- Skills installables au niveau project ou user.

La difficulté n'est pas de lancer Lua; elle est de faire entrer les skills dans une frontière produit.

## Ce Que LuaSkills Apporte

LuaSkills fournit un contrat runtime, pas une simple collection de conventions.

- Chargement de packages skill.
- Découverte et invocation d'entries.
- Strict help trees.
- Injection de contexte runtime.
- Injection des chemins de dépendances.
- Routing des providers SQLite.
- Couches root system, project et user.
- Intégration Rust, C ABI et public `_json` FFI.

## Catégories De Capacité

| Catégorie | Ce que cela permet |
| --- | --- |
| Runtime Core | Charger des skills, recharger des roots, lister des entries et appeler des skills. |
| Skill Authoring | Écrire des skills avec les APIs `vulcan.*` et une aide structurée. |
| Product Control | Garder permissions, budget, UI et authority côté hôte. |
| Data-aware Skills | Connecter SQLite via runtime, host provider ou space controller. |
| Multi-language Hosts | Utiliser le même modèle depuis Rust, C ABI, TypeScript, Python et Go. |

## Exemples Importants

- [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit) : exemple product-grade de LuaSkills.
- [demo-skill](https://github.com/LuaSkills/demo-skill) : template minimal de dépôt skill.

## Conclusion

LuaSkills est une couche runtime qui transforme des packages Lua en skills de produit.
Il rend les skills portables, laisse le contrôle à l'hôte et offre une route commune aux intégrations multilangues.
