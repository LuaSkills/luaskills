# なぜ LuaSkills なのか

[English](../../product/why-luaskills.md) | [简体中文](../../zh-CN/product/why-luaskills.md) | [日本語](why-luaskills.md) | [한국어](../../ko/product/why-luaskills.md) | [Español](../../es/product/why-luaskills.md) | [Français](../../fr/product/why-luaskills.md) | [Deutsch](../../de/product/why-luaskills.md) | [Português (BR)](../../pt-BR/product/why-luaskills.md)

[日本語ドキュメント入口](../index.md)

LuaSkills が解決するのは「Lua を実行する方法」だけではありません。
製品が script、tool、database、AI Agent workflow、ユーザー追加機能を長期的に扱うとき、安定した runtime 境界が必要になります。

## 製品上の課題

多くのホスト製品は最終的に次の能力を必要とします:

- AI Agent 用のローカル tool。
- IDE、デスクトップアプリ、開発者 tool の workflow。
- 検索、memory、database を使う skill。
- 製品同梱の first-party skill。
- project または user level の後追加 skill。

難しいのは Lua を起動することではなく、製品の境界に合わせることです。

## LuaSkills が提供するもの

LuaSkills は口頭の約束ではなく runtime contract を提供します。

- skill package loading。
- entry discovery と invocation。
- strict help tree。
- runtime context injection。
- dependency path injection。
- SQLite provider routing。
- system、project、user root layering。
- Rust、C ABI、public `_json` FFI integration。

## 能力カテゴリ

| カテゴリ | 可能になること |
| --- | --- |
| Runtime Core | skill の読み込み、root reload、entry list、skill call。 |
| Skill Authoring | `vulcan.*` API と構造化 help による skill 作成。 |
| Product Control | 権限、budget、UI、authority をホスト側で制御。 |
| Data-aware Skills | SQLite を runtime、host provider、space controller 経由で扱う。 |
| Multi-language Hosts | Rust、C ABI、TypeScript、Python、Go から同じ skill model に接続。 |

## 重要な例

- [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit): 実際の product-grade skill の参考例。
- [demo-skill](https://github.com/LuaSkills/demo-skill): 最小 skill repository template。

## まとめ

LuaSkills は Lua package を product-grade skill に変える runtime layer です。
skill を移植しやすくし、ホストに制御を残し、多言語統合に共通の道を与えます。
