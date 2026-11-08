# RAG を作る

資料をパースし、全文と位置メタデータをコーパスに保存します。全文から切り出したチャンクを検索し、質問に関係する本文を Ollama に渡して、出典と行番号の付いた回答を得ます。最初はキーワード索引を使い、次に埋め込みを生成してベクトル索引を追加します。

## 1. プロジェクトと資料を用意する

[CLI のセットアップ](../README.md#cli-のセットアップ)を済ませ、リポジトリのルートで実行します。回答を生成するため、Ollama を起動し、プリセットの回答モデル `qwen3:4b` を用意します。別の導入済みモデルを使う場合は `rag.generation.model` を変更します。

```sh
dunlin init ./rag-work --preset rag
dunlin config set --config ./rag-work/dunlin.json rag.embedding null
```

`rag` プリセットは既定で Ollama の埋め込みと SQLite を使います。最初の例では `rag.embedding` を `null` に設定し、キーワード検索で質問します。

`init` が作成した `rag-work/docs/` に UTF-8 の `policy.md` を置きます。設定の `sources.docs` がこのフォルダを指します。

```markdown
# バックアップ方針

バックアップの保管期間は30日です。
```

## 2. 取り込んで質問する

```sh
dunlin ingest --config ./rag-work/dunlin.json
dunlin ask --config ./rag-work/dunlin.json --source docs "バックアップの保管期間は何日ですか？"
```

`ingest` の出力で `docs` の `files` が `1` であることを確認します。`units` はキーワード索引用のチャンク数です。回答には「30日」と引用番号が表示され、続く出典欄で `policy.md` の URL、3行目、引用本文を確認できます。`--json` を付けると `status`、`answer`、`citations` を受け取れます。

`insufficient_evidence` が返る場合は、資料が取り込まれているかと `--source` の指定を確認します。資料と質問で言い回しが異なる場合は `--query "バックアップ 保管期間"` のように検索語を明示できます。

取り込み元の文書を編集したら `ingest` を再実行します。特定の設定済みソースだけ取り込み直すには `ingest --source docs --config ./rag-work/dunlin.json` を使います。そのソースのキーワード索引は現在の文書一式で更新し、コーパスには取り込み済みの各版が残ります。テキストや HTML の原本も保存するには、`dunlin config set --config ./rag-work/dunlin.json keepOriginals.docs true` を実行します。[原本の保存設定](../tools/rag/README.md#文書コーパスと原本の設定)

## 3. ベクトル検索を追加する

Ollama に `embeddinggemma` を用意し、`dunlin.json` の `rag.embedding` の値を `null` から次のオブジェクトに変更します。プリセットの `vectorStore` は SQLite です。必要な拡張を利用できない環境では `vectorStore.type` を `local` に変更できます。

```json
{
  "provider": "ollama",
  "model": "embeddinggemma"
}
```

```sh
dunlin embed --config ./rag-work/dunlin.json
dunlin ask --config ./rag-work/dunlin.json --source docs "保存しておく日数は？"
```

`embed` はベクトル検索用のチャンクを作り、それぞれの埋め込みをベクトル索引に保存します。埋め込みモデルを設定すると `ask` はベクトル検索を使います。`ingest` でキーワード索引が更新されたときや、埋め込みモデル・接続先・保存先を変えたときは `embed` を再実行します。

`dunlin corpus show --config ./rag-work/dunlin.json` で、ソース別の文書数と両方の索引の準備状況を確認できます。`search.vector.state` が `missing` または `stale` の場合は `embed` を実行します。[コーパスと検索の状態](../tools/rag/README.md#コーパスと検索の状態)

回答生成先には質問と検索で得た本文を送ります。埋め込みモデルには、索引作成時はチャンクを、検索時は質問または検索語を送ります。接続先と取り込む資料はそれに合わせて選びます。OpenAI・Strata での回答生成、Chroma・Qdrant へのベクトル索引の保存、S3 や PDF の取り込みは CLI リファレンスに手順があります。[モデル・保存先・パーサーの設定](../tools/rag/README.md)
