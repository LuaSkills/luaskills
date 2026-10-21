# LuaSkills ドキュメント

[English](../index.md) | [简体中文](../zh-CN/index.md) | [日本語](index.md) | [한국어](../ko/index.md) | [Español](../es/index.md) | [Français](../fr/index.md) | [Deutsch](../de/index.md) | [Português (BR)](../pt-BR/index.md)

[日本語 README](../../README.ja.md) | [LuaSkills を選ぶ理由](product/why-luaskills.md)

ドキュメントは目的別に「スキル開発」「ホスト統合」「SDK」の 3 つに分かれています。技術ドキュメントは英語、ホストと FFI の詳細リファレンスは中国語で管理しています。

## 読み始める場所

| 目的 | 入口 |
| --- | --- |
| LuaSkills を評価する | [LuaSkills を選ぶ理由](product/why-luaskills.md) |
| Lua スキルを書く | [Skill development manual](../development/skill-development.md) |
| System Plugin や管理された Python/Node 処理を書く | [System Plugin managed runtime guide](../development/system-plugin-managed-runtime.md) |
| Rust ホストに組み込む | [Runtime architecture](../integration/runtime-model.md) |
| C ABI または JSON FFI で組み込む | [FFI and integration overview](../integration/ffi-overview.md) |
| TypeScript・Python・Go を使う | [SDK overview](../sdk/index.md) |

## スキル開発

- [Skill development manual](../development/skill-development.md)
- [System Plugin managed runtime guide](../development/system-plugin-managed-runtime.md)
- [demo-skill](https://github.com/LuaSkills/demo-skill)：最小のスキルテンプレート。
- [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit)：実運用レベルの例。

### スキル命名規則

`skill_id` とすべての `entry.name` は `^[a-z]([a-z0-9-]*[a-z0-9])?$` に一致する必要があります。
物理的なスキルディレクトリ名が唯一の `skill_id` であり、`skill.yaml` に `skill_id` フィールドを宣言してはいけません。
canonical entry は `{skill_id}-{entry_name}` として公開され、衝突時には安定した `-N` 接尾辞が付く場合があります。
GitHub 管理のスキルでは、リポジトリ由来または明示的な `skill_id`、リリース zip の接頭辞、checksum の接頭辞、zip の最上位ディレクトリ、インストール先ディレクトリが一致する必要があります。
リリース資産は `{skill_id}-v{version}-skill.zip` と `{skill_id}-v{version}-checksums.txt` を使い、zip には `{skill_id}/skill.yaml` を含めます。

## ホスト統合

- [Runtime architecture](../integration/runtime-model.md)
- [FFI and integration overview](../integration/ffi-overview.md)
- [Host-selected managed runtime roots](../integration/managed-runtime-host-roots.md)
- [Package configuration host contract](../integration/package-configuration.md)
- [Database providers](../integration/database-providers.md)
- [中国語の詳細リファレンス](../zh-CN/index.md#宿主对接)

## SDK

- [SDK overview](../sdk/index.md)
- [luaskills-sdk-typescript](https://github.com/LuaSkills/luaskills-sdk-typescript) · [luaskills-sdk-python](https://github.com/LuaSkills/luaskills-sdk-python) · [luaskills-sdk-go](https://github.com/LuaSkills/luaskills-sdk-go)

## ローカル例

- [Rust デモ](../../examples/demo-rust/README.md)
- [C FFI デモ](../../examples/ffi/c/README.md)
- [TypeScript FFI デモ](../../examples/ffi/typescript/README.md)
- [Standard Runtime Fixture](../../examples/ffi/standard_runtime/README.md)
- [Host Provider Demo](../../examples/ffi/host_provider_demo/README.md)
