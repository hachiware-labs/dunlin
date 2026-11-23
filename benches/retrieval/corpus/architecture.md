# Knowledge-Nest の実装構成

CLI のコマンドの解釈から、`KnowledgeNestfile.yaml` の読み込みと検査、各段階の実行、表示までを Rust で実装しています。npm の入口（`tools/rag/cli.mjs`）は、Node.js API の `KnowledgeNest.runCli` を呼んで終了コードを返すだけの起動役です。

| 部分 | 担当 |
| --- | --- |
| `tools/rag/cli.mjs` | npm の入口。引数を Rust に渡し、サーバーの動作中だけ SIGINT と SIGTERM で止める |
| `src/cli_*.rs` | コマンドの解釈と表示、対象プロジェクトの特定、設定の読み込みと検査（Wiki・PARA を含む）、初期化、利用者設定、ツールと OCR の準備、辞書 |
| `src/registry.rs`、`src/config_check.rs`、`src/yaml_*.rs` | モジュールの登録表と引数の検査、`KnowledgeNestfile.yaml` の読み書き |
| `src/*_stage.rs`、`src/eval_*.rs`、`src/serve.rs` | 構築の段階、検索、評価とレポート、試行と採用、HTTP・MCP の提供 |
| `src/source_load.rs`、`src/html_text.rs`、`src/office.rs` | 原本の読み込みとパース（テキスト、HTML、Office、PDF、OCR、外部コマンド） |
| `src/wiki_*.rs`、`src/markdown.rs` | Wiki・PARA のページの生成、引用の検証、原本・出典・ページ版の管理、HTML 化、作業の進捗、画面 |
| `js/index.ts`、`src/node.rs` | TypeScript の API と N-API 境界。Rust の索引処理や検索は非同期のワーカーから実行 |
| `src/engine.rs`、`src/japanese.rs`、`src/corpus.rs` | Tantivy の BM25、Lindera/IPADIC のトークナイザ、版を持つコーパス |
| `src/vector.rs`、`tools/hannoy/` | `f32` の厳密ベクトル検索と、選択時に使う任意の近似ベクトル索引 |

## プロジェクト内の保存先

| 場所 | 主な内容 |
| --- | --- |
| `KnowledgeNestfile.yaml` | プロジェクト種別、モジュール、モデル参照、提供設定 |
| `.knowledge-nest/state.sqlite` | 構築段階の結果、ビルド、タグ、採用履歴、評価データやレポート、Wiki・PARA の状態 |
| `.knowledge-nest/corpus/` | 原文から抽出した本文、位置、版 |
| `.knowledge-nest/artifacts/` | 保存対象の原本（構築では、テキスト文書の原本はコーパスの本文から復元するので置かない）、ビルドごとの索引、ベクトル、構築時の辞書 |
| `raw/` | Wiki の原本。人が集めた資料と、画面から上げたファイル、URL から取った資料。AI は読むだけ |
| `wiki/`（PARA は `site/`）、`rules.md` | Wiki・PARA のページ（HTML。各ファイルの末尾の JSON がページの内容で、ほかに控えはない）、目録の `index.html`、作業の記録の `log.html`、運用の決まり |

入力の識別子と段階の設定を使い、抽出やチャンク化の結果を再利用します。差分のキーワード索引では、可能な環境で変更されない Tantivy セグメントをハードリンクで共有します。各ビルドは作成時のユーザー辞書を参照します。

## 稼働中の検索が読む索引

`serve` は起動時の検索設定を使います。同じ設定で資料を更新した場合は、構築が正常に終わってから新しいビルドを開き、後続の要求で再利用します。タグ付き試行は `search --build` で比較でき、`use` で採用した設定は `serve` の再起動時に反映します。

CLI、HTTP、MCP の検索は同じ検索と文脈の組み立て処理を通ります。RAG の `ask` と `/v1/responses` は、その文脈をモデルに渡して出典付きの回答を組み立てます。 [構築の再利用](knowledge-nest-spec.md#7-再利用の仕組みと識別子) · [検索と回答の共通形式](mcp.md#検索と回答の-json)
