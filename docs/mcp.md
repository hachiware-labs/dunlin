# MCP サーバーを作る

ローカル資料の検索と根拠取得を、stdio 対応の MCP クライアントに公開します。以下は検索専用の構成です。PARA や LLM Wiki を使う場合も、同じ方法で各プロジェクトの `dunlin.json` をクライアントに指定します。

## 1. 検索する資料を登録する

[CLI のセットアップ](../README.md#cli-のセットアップ)を済ませ、リポジトリのルートで実行します。

```sh
dunlin init ./mcp-work --preset mcp
```

`init` が作成した `mcp-work/docs/` に UTF-8 の `policy.md` を置きます。

```markdown
# バックアップ方針

バックアップの保管期間は30日です。
```

```sh
dunlin ingest --config ./mcp-work/dunlin.json
```

`ingest` の出力で `sourceId` が `docs`、`files` が `1` であることを確認します。`ingest` はパース後の全文をコーパスに保存し、チャンクからキーワード索引を作ります。`dunlin corpus show --config ./mcp-work/dunlin.json` でも、参照先と文書数、キーワード索引の状態を確認できます。

## 2. MCP クライアントに登録する

クライアントの stdio サーバー設定で、実行コマンドを `dunlin`、引数を「`serve`」「`--stdio`」「`--config`」「設定ファイルの絶対パス」の順で指定します。`mcpServers` 形式のクライアントでは次のように設定できます。パスは自分の環境に置き換えます。

```json
{
  "mcpServers": {
    "dunlin": {
      "command": "dunlin",
      "args": [
        "serve",
        "--stdio",
        "--config",
        "C:/workspace/dunlin/mcp-work/dunlin.json"
      ]
    }
  }
}
```

クライアントがサーバープロセスを起動します。`dunlin` が見つからない場合は、クライアントが npm の実行ファイル用フォルダを `PATH` に含めているかを確認します。Windows で npm のコマンドを直接起動できないクライアントでは、`command` に `node.exe` の絶対パスを指定し、`args` の先頭に `tools/rag/cli.mjs` の絶対パスを追加します。検索専用サーバーに回答モデルや埋め込みモデルの設定は不要です。

## 3. 接続と検索を確認する

1. クライアントのツール一覧で `knowledge.search` と `knowledge.read` が表示されることを確認します。
2. `knowledge.search` を `{"query":"バックアップ","sourceIds":["docs"]}` で呼び出します。
3. `hits` の該当項目から `evidence` を取り出し、`knowledge.read` の `evidence` 引数に渡します。`policy.md` の URL、行範囲、保管期間の本文を確認します。

検索専用の構成では、Markdown のリンクをたどる `knowledge.explore` と、Node.js API で作った抜粋型 Wiki を読む `knowledge.wiki_get` も使えます。取り込み元の文書を変更したら同じ設定で `ingest` を再実行します。検索と本文取得の間にキーワード索引が更新された場合は、検索し直して現在の版の `evidence` を取得します。[Node.js の MCP API](../README.md#mcp)

## コーパスと索引を複数の用途で使う

同じ `index` を参照する設定間では、文書と索引に加え、コーパスの名称やメタデータも共有されます。[コーパスの名称とメタデータ](../tools/rag/README.md#コーパスの名称とメタデータ)

[RAG の手順](rag.md)で作った `rag-work` に PARA と LLM Wiki の保存先を追加できます。`authoring.sourceRoot` に指定する `rag-work/docs/` は先に作成しておきます。

```sh
dunlin config set --config ./rag-work/dunlin.json authoring.para ./para
dunlin config set --config ./rag-work/dunlin.json authoring.wiki ./wiki
dunlin config set --config ./rag-work/dunlin.json authoring.sourceRoot ./docs
```

MCP クライアントには `rag-work/dunlin.json` を指定します。`authoring.para` または `authoring.wiki` があると、CLI は編集用 MCP を起動し、設定した保存先に応じて `para.*` と `wiki.*` を公開します。検索専用のツールセットも使う場合は、同じ `index` ディレクトリを参照し、`authoring` を省いた別の設定ファイルをクライアントに登録します。

すでに `ingest` した文書は、編集用 MCP からそのまま検索して引用できます。`knowledge.search` の結果から `evidence.source` の `sourceId`、`objectId`、`version` と `evidence.range` の `startLine` を取り出し、`source.read` に渡すと、コーパスに保存した本文を該当箇所から読めます。公開した HTML ページの本文も検索に含めたい場合は、ファイルを `source.upload` で取り込めます。RAG でベクトル検索を使う場合は、取り込み後に `embed` でベクトル索引を更新します。

MCP クライアントは接続先の索引全体を検索できます。共有範囲を分けるときは索引と設定を分けます。`authoring.sourceRoot` は `source.ingest` で読めるローカルファイルの範囲を指定します。編集用 MCP はノートの保存や資料の取り込みも行えるため、信頼できるクライアントに接続します。

編集用 MCP の `source.read` は、`version` を指定するとコーパスに保存したその版の本文を読みます。原本が保存されていれば、`evidence.original.uri` を `resources/read` に渡すと原本のバイト列を取得できます。[コーパスと原本の設定](../tools/rag/README.md#文書コーパスと原本の設定)
