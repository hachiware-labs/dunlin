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

## モジュールとリファレンスを更新する

MCP・RAG のモジュールは `tools/rag/project-modules.mjs` の登録箇所に、配置する区切り、役割、入出力、引数、必要なツールやモデルを定義します。型と配置に関する検査は `validate` から確認します。新規プロジェクトの既定値を変えた場合は、4種別の初期化結果も確認します。

CLI や初期設定を変更したら、リファレンスを再生成します。

```sh
node scripts/generate-docs.mjs
node scripts/generate-docs.mjs --check
```

変更を共有する場合は、利用者に見える差と、実行した確認を記載します。仕様や既存プロジェクトへの影響は該当するガイドにも反映します。 [実装構成](docs/architecture.md) · [受け入れ確認](docs/acceptance.md)
