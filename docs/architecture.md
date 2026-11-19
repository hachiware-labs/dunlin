# Dunlin の実装構成

Node.js のプロジェクト CLI が `Dunlinfile.yaml` を読み、処理順とモデル接続を決めます。索引・検索・コーパスの中核は Rust で実装し、TypeScript の API を通じて呼び出します。

| 部分 | 担当 |
| --- | --- |
| `tools/rag/cli.mjs`、`project.mjs` | コマンドの解析、対象プロジェクトの特定、構築・試行・採用、HTTP・MCP の起動 |
| `project-modules.mjs`、`project-content.mjs` | YAML の各区切りの検証、モジュール順、入力の走査、抽出とチャンク化 |
| `project-eval.mjs`、`project-dictionary.mjs` | 評価データを選んだときの設定の書き換え、ユーザー辞書の提案と候補設定への反映。評価、引用の位置対応、レポートの比較は Rust（`src/eval_*.rs`） |
| `project-wiki.mjs` | Wiki・PARA の設定の検証。ページの生成、引用の検証、原本・出典・ページ版の管理、HTML 化、作業の進捗、画面は Rust（`src/wiki_*.rs`、`src/markdown.rs`） |
| `js/index.ts`、`src/node.rs` | TypeScript の API と N-API 境界。Rust の索引処理や検索は非同期のワーカーから実行 |
| `src/engine.rs`、`src/japanese.rs`、`src/corpus.rs` | Tantivy の BM25、Lindera/IPADIC のトークナイザ、版を持つコーパス |
| `src/vector.rs`、`tools/hannoy/` | `f32` の厳密ベクトル検索と、選択時に使う任意の近似ベクトル索引 |

## プロジェクト内の保存先

| 場所 | 主な内容 |
| --- | --- |
| `Dunlinfile.yaml` | プロジェクト種別、モジュール、モデル参照、提供設定 |
| `.dunlin/state.sqlite` | 構築段階の結果、ビルド、タグ、採用履歴、評価データやレポート、Wiki・PARA の状態 |
| `.dunlin/corpus/` | 原文から抽出した本文、位置、版 |
| `.dunlin/artifacts/` | 保存対象の原本、ビルドごとの索引、ベクトル、構築時の辞書 |
| `site/`、`rules.md` | Wiki・PARA の生成済み HTML と運用の決まり |

入力の識別子と段階の設定を使い、抽出やチャンク化の結果を再利用します。差分のキーワード索引では、可能な環境で変更されない Tantivy セグメントをハードリンクで共有します。各ビルドは作成時のユーザー辞書を参照します。

## 稼働中の検索が読む索引

`serve` は起動時の検索設定を使います。同じ設定で資料を更新した場合は、構築が正常に終わってから新しいビルドを開き、後続の要求で再利用します。タグ付き試行は `search --build` で比較でき、`use` で採用した設定は `serve` の再起動時に反映します。

CLI、HTTP、MCP の検索は同じ検索と文脈の組み立て処理を通ります。RAG の `ask` と `/v1/responses` は、その文脈をモデルに渡して出典付きの回答を組み立てます。 [構築の再利用](dunlin-spec.md#7-再利用の仕組みと識別子) · [検索と回答の共通形式](mcp.md#検索と回答の-json)
