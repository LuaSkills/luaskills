<div align="center">

# LuaSkills

**Lua スクリプトをプロダクト品質のスキルに変える Rust ランタイム**

Rust コア · LuaJIT スキル · Rust API、C ABI、JSON FFI、TypeScript / Python / Go SDK

[![crates.io](https://img.shields.io/crates/v/luaskills.svg)](https://crates.io/crates/luaskills)
[![docs.rs](https://img.shields.io/docsrs/luaskills)](https://docs.rs/luaskills)
[![License: MIT](https://img.shields.io/badge/License-MIT-green.svg)](LICENSE)
[![Release](https://img.shields.io/github/v/release/LuaSkills/luaskills)](https://github.com/LuaSkills/luaskills/releases)

[English](README.md) | [简体中文](README.zh-CN.md) | **日本語** | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (BR)](README.pt-BR.md)

[ドキュメント](docs/ja/index.md) · [スキルテンプレート](https://github.com/LuaSkills/demo-skill) · [CodeKit の例](https://github.com/LuaSkills/vulcan-codekit)

</div>

> これは[英語版 README](README.md) の翻訳です。内容が異なる場合は英語版が正となります。

---

## 概要

LuaSkills はホストアプリケーションの中で Lua スキルパッケージを読み込み、実行し、管理します。ホストは独自のプラグインランタイムを作らなくても、スクリプト化できるツール、構造化ヘルプ、管理された Python と Node.js ランタイム、依存関係管理、データベース対応の実行を利用できます。

**LuaSkills はスキルを実行し、それをどうプロダクト機能にするかはホストが決めます。** ポリシー、認証、権限確認、予算、保存場所、ユーザー体験はホスト側に残ります。

LuaSkills は [Vulcan Code](https://github.com/OpenVulcan/vulcan-code) のスキルシステムを支えています。単発の組み込みスクリプトではなく、制御されたスキルシステムが必要な AI エージェント、IDE、デスクトップやサーバー製品に適しています。

## 主な機能

### 🧩 スキルランタイム

- `ROOT`・`PROJECT`・`USER` の階層ルートでスキルを検出、読み込み、一覧、呼び出し、再読み込み
- ドキュメント、ツール説明、コマンドパレット、UI パネルとして描画できる厳格なヘルプツリー
- エントリごとの完全なオブジェクト入力スキーマと、正規名 `{skill_id}-{entry_name}`
- GitHub Release や URL からのインストール・更新・アンインストール、チェックサム検証付きの依存関係

### 🧰 標準 Lua API

- `vulcan.fs`、`vulcan.path`、`vulcan.process`、`vulcan.io`、`vulcan.os`、`vulcan.json`、`vulcan.cache`
- リクエスト・クライアント・予算・スキルディレクトリ情報の `vulcan.context`、依存ルートの `vulcan.deps`
- `vulcan.host.*` でホスト登録ツールを、`vulcan.models.*` でホスト管理の埋め込みモデルと LLM を利用
- `vulcan.config.*` はパッケージごとに型付き・検証済みの設定を、バージョン付きストアと変更イベントで提供

### 🐍 管理された Python と Node.js

- `vulcan.runtime.python.*` と `vulcan.runtime.node.*` からバージョン指定の Python / Node.js ハンドラーを呼び出し
- ロックされた依存関係、上限付き出力、確実なプロセスツリーの後始末
- 厳格な System Plugin リースでは呼び出しをまたぐ永続セッション、ホストイベント、エッジトリガーのウェイクコールバック
- ホストが指定する読み取り専用の配布ルートと書き込み可能な環境ルート

### 🔌 ホスト統合

- Rust ホスト向けの crate API
- その他の言語向けの標準 C ABI と公開 `_json` FFI
- 永続 Lua VM のための公開 `runtime_lease` と権限に結び付いた `system_runtime_lease`
- 構造化された `host_result` の返却。最初の種類は IDE 品質の `change_set` 編集
- 動的ライブラリ、ホストコールバック、space controller による SQLite / LanceDB（任意）

### 📦 SDK

- JSON FFI 上の [TypeScript / Node.js](https://github.com/LuaSkills/luaskills-sdk-typescript)、[Python](https://github.com/LuaSkills/luaskills-sdk-python)、[Go](https://github.com/LuaSkills/luaskills-sdk-go) SDK
- SHA-256 検証付きのランタイム資産インストール

## LuaSkills ではないもの

- 単体の MCP サーバーではありません。MCP ホスト層は [vulcan-mcp](https://github.com/OpenVulcan/vulcan-mcp) が提供します
- ホスト設定の読み込み、予算計算、UI 描画は行いません
- **サンドボックスではありません。** スキルは信頼済みコードとして扱われます。有効にするルート、インストールするスキル、公開する管理操作、許可するデータベースモード、各操作の権限はホストが決めます。

## アーキテクチャ

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

| ディレクトリ | 説明 |
| --- | --- |
| `src/runtime` | エンジン、Lua VM、リース、コンテキスト、ヘルプ、結果、管理ランタイム |
| `src/skill` | マニフェスト、ルート、設定、ライフサイクル管理 |
| `src/dependency`、`src/download` | 依存関係の解決、ダウンロード、アーカイブ展開 |
| `src/providers` | SQLite と LanceDB のバインディング |
| `src/ffi`、`src/ffi_standard`、`src/ffi_embedded` | 公開 `_json` FFI、標準 C ABI、組み込みランタイム転送 |
| `include/` | FFI ホスト向け C ヘッダー |
| `contracts/` | SDK と共有する機械可読な契約 |
| `examples/` | Rust、FFI、管理ランタイム、設定の例 |

## はじめに

### Rust ホスト

```toml
[dependencies]
luaskills = "0.7.1"
```

[`examples/demo-rust`](examples/demo-rust/README.md) に `call_skill` と `vulcan.host.*` ツールブリッジの例があります。

### その他の言語

| ホスト | 方法 |
| --- | --- |
| TypeScript / Node.js | `npm install @luaskills/sdk` |
| Python | `pip install luaskills-sdk` |
| Go | `go get github.com/LuaSkills/luaskills-sdk-go` |
| C / C++ | [Releases](https://github.com/LuaSkills/luaskills/releases) から `luaskills-ffi-sdk-{platform}.tar.gz` を取得し、標準 C ABI を使用 |

[SDK overview](docs/sdk/index.md) と [FFI overview](docs/integration/ffi-overview.md) を参照してください。

### スキルを書く

[demo-skill](https://github.com/LuaSkills/demo-skill) から始め、[Skill development manual](docs/development/skill-development.md) を読んでください。実際のランタイム構成で 1 つのスキルをデバッグするには：

```bash
cargo run --bin luaskills-debug -- call \
  --runtime-root D:/runtime \
  --skill-path D:/skills/demo-skill \
  --tool ping \
  --args-json "{\"note\":\"hello\"}"
```

`luaskills-debug` はスキルを `runtime_root/skills/<skill_id>` にコピーし、通常の `load_from_roots -> call_skill` 経路で実行するため、依存ルート、状態、データベース、コンテキストが実際のホスト実行と一致します。リポジトリ内の開発ツールであり、SDK や FFI の API は追加しません。

## ドキュメント

| 分類 | ドキュメント |
| --- | --- |
| スキル開発 | [Skill development manual](docs/development/skill-development.md) · [System Plugin managed runtime guide](docs/development/system-plugin-managed-runtime.md) |
| ホスト統合 | [Runtime architecture](docs/integration/runtime-model.md) · [FFI overview](docs/integration/ffi-overview.md) · [Managed runtime roots](docs/integration/managed-runtime-host-roots.md) · [Package configuration](docs/integration/package-configuration.md) · [Database providers](docs/integration/database-providers.md) |
| SDK | [SDK overview](docs/sdk/index.md) |
| すべて | [日本語ドキュメント](docs/ja/index.md) · [Documentation hub](docs/index.md) |

## エコシステム

| リポジトリ | 説明 |
| --- | --- |
| [vulcan-codekit](https://github.com/LuaSkills/vulcan-codekit) | コードナビゲーション、AST 検査、構造検索、Markdown ナビゲーション、安全なパッチ適用 |
| [vulcan-curl](https://github.com/LuaSkills/vulcan-curl) | 構造化された HTTP GET / POST リクエスト |
| [vulcan-file](https://github.com/LuaSkills/vulcan-file) | 無視ルールを考慮した一覧、正確な読み取り、プレビュー優先の小さな編集 |
| [vulcan-lua](https://github.com/LuaSkills/vulcan-lua) | 制限付きのインラインまたはファイル単位の Lua 実行 |
| [vulcan-testkit](https://github.com/LuaSkills/vulcan-testkit) | ビルド、テスト、lint、型チェックの出力を簡潔な診断に変換 |
| [vulcan-workmem](https://github.com/LuaSkills/vulcan-workmem) | チェックポイントと引き継ぎのためのプロジェクト単位の作業メモリ |
| [demo-skill](https://github.com/LuaSkills/demo-skill) | 最小のスキルリポジトリテンプレート |
| [luaskills-packages](https://github.com/LuaSkills/luaskills-packages) | Lua ランタイムパッケージとネイティブ依存バンドル |

## バージョン

| コンポーネント | バージョン |
| --- | --- |
| LuaSkills コアと FFI SDK | `0.7.1` |
| TypeScript・Python・Go SDK | 独立したバージョン系列。[SDK overview](docs/sdk/index.md#versions) を参照 |
| Lua ランタイムパッケージ | 互換 `0.1` 系列 |
| 管理 Python / uv | `3.14.6` / `0.11.28` |
| 管理 Node.js / pnpm | `24.18.0` / `11.11.0` |
| 管理ランタイムの既定値 | `workers=4 / idle=60s / sessions=256 / buffer=1MiB/stream / invoke=unlimited` |
| vldb-controller | `0.2.3` |
| vldb-sqlite / vldb-lancedb | `0.1.6` / `0.1.5` |

スクリプトの既定値は `scripts/deps/` にあります。リリースは[リリース手順](scripts/release/README.md)に従い、ランタイムパッケージ、5 プラットフォームのコアリリース、crate、SDK の順に進みます。

## コントリビュート

Issue と Pull Request を歓迎します。

1. リポジトリを Fork し、`main` からブランチを作成します。
2. ローカルで検証します：

   ```bash
   cargo fmt --all -- --check
   cargo clippy --all-targets -- -D warnings
   cargo test --all-targets -- --test-threads=1
   cargo deny check
   ```

3. すべてのコミットに `git commit -s` で署名します。DCO は [CONTRIBUTING.md](CONTRIBUTING.md) を参照してください。
4. 各 PR は 1 つの目的に絞り、何を・なぜ変更し、どう検証したかを説明してください。

## ライセンス

[MIT](LICENSE)。サードパーティの表記は [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) にあります。
