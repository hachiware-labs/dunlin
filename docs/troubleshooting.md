# 困ったときは

失敗した CLI コマンドに `--json` を付けると、`errors` の `code` と `message` を確認できます。設定の診断では `file`、`line`、`hint` も表示されます。`serve` では `--print-config` の表示に `--json` を使えます。 [CLI リファレンス](cli.md)

| 症状 | 確認と対処 |
| --- | --- |
| `nest` が見つからない | リポジトリで `npm link` を実行します。`npm prefix -g` で確認した場所を Windows では `PATH` に追加します。macOS・Linux ではその配下の `bin` を追加します。 |
| `PROJECT_REQUIRED` | `KnowledgeNestfile.yaml` のあるディレクトリへ移動するか、`--project <directory>` を指定します。 |
| `BUILD_REQUIRED` | MCP・RAG の資料を作業ディレクトリの `docs/` または設定したソースに置き、`nest build` を実行します。 |
| `BUILD_NOT_FOUND` | `--build` や `release` に渡したタグまたは ID のビルドがありません。`nest status` で現在のビルドを、`nest tag` で付けたタグを確認します。 |
| `SOURCE_MISSING` | 資料のフォルダがありません。`init` では `--source` に既存のフォルダを渡します。`build` では、フォルダを作り直すか、`KnowledgeNestfile.yaml` の `source.filesystem` の `path` を直します。 |
| `UNSUPPORTED_FILES` | 一覧のファイルを確認します。MCP・RAG では `source.filesystem` の `with.exclude` を設定するか、`nest build --skip-unsupported` を使います。Wiki・PARA では `ingress` の `loader.auto` で `unsupported: skip` を選べます。 |
| PDF から文字が取得できない | PDF に文字層があるかを確認します。スキャン資料を読む場合は `nest add ocr` の案内に従い、OCR を準備します。 |
| `MODEL_REQUIRED` | `KnowledgeNestfile.yaml` の RAG では `models.answerer.model`、Wiki・PARA では `models.writer.model` を設定します。稼働中の `serve` にモデル設定を反映するときは起動し直します。 |
| モデルへの接続に失敗する | モデルの名前、提供元、`base_url` を確認します。Ollama は起動とモデルの取得を確認します。API キーを使う提供元では、起動している端末の `api_key_env` に対応する環境変数を確認します。 |
| 埋め込みの要求で `build` が止まる | 使う埋め込みモデルの接続を確認します。これからキーワード検索用に初期化する場合は `init` に `--no-embedding` を指定します。 |
| Wiki・PARA の取り込みで `INVALID_CITATION` | モデルが書いた記述の引用が、どれも資料と一字一句一致しませんでした。もう一度 `ingest` を実行するか、より大きなモデルを使います。一部の記述だけが一致しなかった場合は、残りを保存し、保存しなかった記述を `CITATION_REJECTED` の警告で示します。 |
| `BUILD_BUSY` または `DB_BUSY` | 同じプロジェクトの更新処理が終わってから再試行します。Wiki・PARA は作業 ID を `nest progress` に渡すと進捗を確認できます。 |
| `EVAL_DATA_REQUIRED` または `REPORT_REQUIRED` | `eval-data add` または `import` で調整用の評価データを入れ、`nest eval --tag baseline` を実行します。 |
| `INVALID_USER_DICTIONARY` | UTF-8 の見出しなし CSV に `表層形,品詞,読み` の3列を並べます。表層形の空白と、候補設定から見た辞書の相対パスを確認します。 |
| HTTP の `INVALID_JSON` | 本文を UTF-8 の JSON で送ります。Windows の `curl` で日本語を直接書くと UTF-8 にならないことがあります。JSON を UTF-8 のファイルに保存し、`curl --data-binary @request.json` で送ります。 |
| HTTP の `MODEL_MISMATCH` | `/v1/responses` の `model` を省くか、メッセージに示されたプロジェクトのモデル名にします。 |
| `AUTH_TOKEN_REQUIRED` または HTTP 401 | `serve.access.auth.token_env` に指定した環境変数を設定し、クライアントの認証情報を合わせます。 |
| HTTP 403 | 接続先のホスト名とブラウザの Origin を `allowed_hosts`、`allowed_origins` と照合します。`CLIENT_FORBIDDEN` のときは、接続元のアドレスが `allowed_clients` の範囲に入っているか確かめます。 |
| ポートが使用中 | 既存の `serve` を止めるか、`nest serve --port 4320` のように空きポートを指定します。 |

HTTP の既定接続先は RAG が `127.0.0.1:4318`、Wiki・PARA が `127.0.0.1:4319` です。モデルの設定とアクセス制限の例は起動ガイドにあります。 [モデルの設定](mcp.md#初期化時にモデルを選ぶ) · [HTTP の接続制限](mcp.md#http-の接続制限)
