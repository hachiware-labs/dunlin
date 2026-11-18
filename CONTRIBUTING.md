# 開発と変更の確認

ソースからのビルドには Rust 1.94.1 と Node.js 22.20 以降の22系、または24.12以降の24系を使います。Windows、Ubuntu、macOS 向けのビルドを扱っています。

```sh
npm ci
npm run build
npm --prefix tools/import ci
npm --prefix tools/rag ci
```

CLI を端末で使う場合はリポジトリのルートで `npm link` を実行します。MCP と RAG の確認には、サンプル資料を使った手順があります。 [クイックスタート](docs/quickstart.md)

## ローカルで確認する

```sh
cargo fmt --all -- --check
cargo fmt --manifest-path tools/hannoy/Cargo.toml -- --check
cargo clippy --locked --all-features --all-targets -- -D warnings
cargo test --locked
npm test
npm --prefix tools/rag test
```

パッケージの変更では `npm run test:package` を使います。バンドル版と分割版をインストールして、検索と出典本文の取得まで試します。GitHub Actions は依頼されたときに手動実行します。**Verify Dunlin** の標準実行は Windows と Ubuntu を、macOS のオプションは ARM と Intel を検証します。

## リリースを準備する

公開する npm パッケージは、ネイティブコードを含まない本体と、プラットフォームごとのパッケージ（アドオン、索引ツール、ライセンス）に分かれます。GitHub Actions の **Release Dunlin** を手動実行すると、4つのプラットフォームでビルドとテストを行い、tarball を成果物 `dunlin-release` にまとめます。`publish` を選ばない限り公開は行いません。

1台で構成だけを確かめる場合は、ビルド済みの `dist` を入力にします。出力先は `dist/release/` です。

```sh
npm run release:prepare -- --artifacts dist --partial
```

公開の前に次を済ませます。

- `package.json` の `name` を確定し、`private` を外す。`server.json` の `identifier`、`name`、バージョンと、`package.json` の `mcpName` を合わせる。食い違いは `release:prepare` が検出します。
- リポジトリのシークレット `NPM_TOKEN` に npm の公開用トークンを登録する。
- **Release Dunlin** を `publish` 付きで実行する。プラットフォームのパッケージを先に、本体を最後に公開します。

MCP Registry への登録は、npm への公開後に `mcp-publisher login github` と `mcp-publisher publish` をリポジトリのルートで実行します。Registry は npm 上のパッケージの `mcpName` で所有を確認します。

## モジュールとリファレンスを更新する

MCP・RAG のモジュールは `tools/rag/project-modules.mjs` の登録箇所に、配置する区切り、役割、入出力、引数、必要なツールやモデルを定義します。型と配置に関する検査は `validate` から確認します。新規プロジェクトの既定値を変えた場合は、4種別の初期化結果も確認します。

CLI や初期設定を変更したら、リファレンスを再生成します。

```sh
npm run docs
npm run docs:check
```

変更を共有する場合は、利用者に見える差と、実行した確認を記載します。仕様や既存プロジェクトへの影響は該当するガイドにも反映します。 [実装構成](docs/architecture.md) · [受け入れ確認](docs/acceptance.md)
