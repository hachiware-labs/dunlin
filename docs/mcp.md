# MCP と RAG の起動

MCP は検索結果と出典をエージェントに渡します。RAG は同じ検索結果を使い、設定したモデルで出典付きの回答を生成します。[生成される KnowledgeNestfile.yaml](#初期化で生成される-knowledgenestfileyaml)

## MCP で検索を提供する

[インストール](../README.md#インストール)を済ませ、`./manuals` に Markdown または UTF-8 テキストの資料を置きます。

```sh
nest init mcp ./mcp --source ./manuals
cd mcp
nest build
nest search "手順" --json
nest serve --print-config
```

最後のコマンドは、MCP クライアントの設定（`.mcp.json` や `claude_desktop_config.json` など）の `mcpServers` にそのまま貼れる項目を表示します。`--json` を付けると、`command` と `args` だけを返します。クライアントが `nest serve` を起動し、`knowledge.search` と `knowledge.read` が使えます。標準のキーワード検索は、モデルや評価データの設定なしで使えます。

資料を更新したら `nest build` を再実行します。実行中の MCP は次の要求から完成した索引を使います。未対応のファイルは `source.filesystem` の `with.exclude` に除外パターンを指定するか、`nest build --skip-unsupported` で飛ばせます。

`build` は `source → transform → index` を実行します。各段階は同名のコマンドでも実行できます。チャンクの長さを変更するには `chunk.length` の `with.size` と `with.overlap` を編集します。`nest build --from transform` で保存済みの抽出結果から再開できます。 [KnowledgeNestfile の設定と既定値](config.md)

`nest build --dry-run` で変更された文書を確認できます。候補設定の試行は `nest build --from transform --config candidate.yaml --tag trial` で作り、`nest search "手順" --build trial` で確認できます。採用には `nest release trial` を実行して MCP を再起動します。入力の版を試行間でそろえる場合は `--input <build>` を指定します。[試行と採用](knowledge-nest-spec.md#11-試行採用外部エージェントによる最適化)

### 以前の init rag プロジェクトを移行する

以前の `init rag` が作った stdio 専用のプロジェクトを使っている場合は、`KnowledgeNestfile.yaml` の `init.type` を `mcp` に変更します。`ask` の区切りがなく、`serve` が `transport: stdio` だけの旧設定は、変更前も MCP として動作します。

## RAG で回答と検索を提供する

`./manuals` のあるディレクトリで初期化します。

```sh
nest init rag ./rag --source ./manuals
cd rag
```

`init` で回答モデルを指定していない場合は、`KnowledgeNestfile.yaml` の `models.answerer.model` にモデル名を設定します。既定の接続先はローカル Ollama の `http://localhost:11434` です。モデルへ送るリクエストの大きさはモデル定義の `max_input_bytes` で制限できます。

```sh
nest build
nest search "交換周期" --json
nest ask "交換周期は？" --json
nest serve
```

既定では `127.0.0.1:4318` の1ポートで提供します。

| 接続先 | リクエスト |
| --- | --- |
| `POST /v1/responses` | `{"input":"交換周期は？"}`。OpenAI SDK の `baseURL` は `http://127.0.0.1:4318/v1`。 |
| `POST /v1/search` | `{"query":"交換周期"}`。必要に応じて `limit`、`budget_chars`、`source_ids` を指定。 |
| `/mcp` | Streamable HTTP の `knowledge.search`。別のターミナルで `nest serve --print-config` を実行すると登録用 URL を取得できます。 |

HTTP の登録用 URL を出力するには、`mcp` エンドポイントと固定ポートが必要です。クライアントは起動済みの `nest serve` に接続します。

回答モデルの準備前でも `build`、`search`、`serve` を実行でき、HTTP 検索と MCP を利用できます。`/v1/responses` はモデルを設定して `serve` を再起動するまで `503 MODEL_REQUIRED` を返します。`serve` の `endpoints` を `[search, mcp]` にすると、検索を提供する2つのエンドポイントが有効になります。stdio を使う場合は `nest serve --transport stdio` を指定します。

## 検索と回答の JSON

`search --json` の `data`、MCP の `knowledge.search` に `format: "json"` を渡したときの結果、`POST /v1/search` の応答は共通形式です。MCP の既定の結果は、エージェント向けの短いテキスト（見つけた箇所の ref の一覧と、上位の箇所の本文）です。 [MCP 入門](start-mcp.md#2-エージェントにつなぐ)`build_id` と `search_config_id` で実行時の構成を確認できます。`results` に検索結果、`context` に組み立てた文脈と文字数・出典範囲・停止理由が入ります。

件数、文脈予算、対象ソースの上書きはその要求にだけ適用されます。CLI では `--limit`、`--budget-chars`、`--source-id`、HTTP と MCP では `limit`、`budget_chars`、`source_ids` を指定します。応答の `search_config_id` は実効設定を表し、`warnings` に上書きが記録されます。

`ask --json` の `data` と `POST /v1/responses` は Responses API 形式です。HTTP の `input` は質問文字列、または最後のユーザーメッセージを含むメッセージ配列を受け付けます。回答の引用は `output[].content[].annotations`、引用した版と原文上の範囲は `knowledge_nest_citations`、ビルドと検索設定の ID は `metadata` に入ります。引用した本文が裏付けないとして答えから外したモデルの文があれば、その文と理由が `knowledge_nest_rejected` に入ります。 [答えの確かめ方](start-rag.md#しくみ)設定と異なる `model`、ストリーミング、ツール呼び出し、過去の応答の引き継ぎなどを指定した要求は HTTP 400 を返します。[応答形式](knowledge-nest-spec.md#27-search-と-ask-の返却形式種別-rag-の-serve)

## HTTP の接続制限

`serve.access.allowed_hosts` と `serve.access.allowed_origins` で宛先とブラウザの送信元を制限します。`serve.access.allowed_clients` に `192.168.11.0/24` のような範囲を書くと、その範囲とサーバーのマシン自身からの接続だけを受け付けます。ループバックの外で待ち受けるには、`allowed_clients` を家や職場のネットワークの範囲だけにするか、`serve.access.auth.type` を `token` にし、`token_env` に指定した環境変数へトークンを入れます。クライアントは `Authorization: Bearer <token>` を送ります。Wiki/PARA の画面は Basic 認証にも対応し、ユーザー名は `knowledge-nest`、パスワードは同じトークンです。

`serve.limits` の既定は、リクエスト本文 1 MiB、タイムアウト 120 秒、同時要求 8 件です。`--host` と `--port` は起動時だけ上書きできます。ポートが使用中の場合は終了コード 1 を返します。`serve` の設定変更によってビルド ID や検索設定 ID は変わりません。[設定の例とコマンド](serve.md) · [接続制限の仕様](knowledge-nest-spec.md#275-serve-の設定項目)

## 初期化時にモデルを選ぶ

使い続けるモデルは、プロジェクトを作る前に一度登録します。次の例では、Ollama に登録済みのモデル名を使います。

```sh
nest config set embedding --provider ollama --model hf.co/Qwen/Qwen3-Embedding-0.6B-GGUF:Q8_0
nest config set llm --provider ollama --model qwen3:4b
nest config list
nest init rag ./rag
```

グローバル設定の保存先は `~/.knowledge-nest/config.yaml` です。`KNOWLEDGE_NEST_GLOBAL_CONFIG` で保存先を指定でき、`nest config unset embedding` や `nest config unset llm` で登録を解除できます。設定は初期化時に新しいプロジェクトへ書き出されます。

`init mcp` はキーワード検索を初期設定とし、埋め込みや OCR は `--embedding-model` や `--ocr` で指定した場合に有効になります。RAG ではグローバル設定を引き継ぎます。

個別のプロジェクトでは、`init` のオプションで優先する設定を指定します。

| 用途 | モデル・プロバイダー指定 | グローバル設定を省く指定 |
| --- | --- | --- |
| 埋め込み（MCP/RAG） | `--embedding-model <name>`、`--embedding-provider <ollama\|openai>` | `--no-embedding` でキーワード検索 |
| 回答・執筆（RAG/Wiki/PARA） | `--llm-model <name>`、`--llm-provider <ollama\|openai\|strata>` | `--no-llm` でモデル名を未設定にする |

埋め込みを指定すると、`models.embedder`、`index.vector`、`retrieval.vector`、`fusion.rrf` を生成し、キーワード検索とベクトル検索を組み合わせます。RAG の LLM は `models.answerer`、Wiki/PARA の LLM は `models.writer` になります。モデル名を添えずにプロバイダーを変更した場合、以前のプロバイダーのモデル名は継承されません。埋め込みを使う場合はモデル名も指定します。

初期化は設定の書き出しまでです。OpenAI の生成設定に含まれる `api_key_env: OPENAI_API_KEY` は、モデルを実行するときに参照されます。

## 初期化で生成される KnowledgeNestfile.yaml

`--source` を省くと、MCP と RAG はどちらも `./docs` を入力にします。グローバル設定にモデルと OCR を登録していない状態で生成される YAML の全文は、設定リファレンスにあります。 [KnowledgeNestfile の設定と既定値](config.md)

スキャン資料を取り込むプロジェクトでは、初期化時に `--ocr` を指定できます。続けて作るプロジェクトで毎回有効にするには `nest config set ocr` を使います。 [OCR の初期化設定](knowledge-nest-spec.md#初期化時の-ocr-指定)

## 日本語検索とユーザー辞書

標準のキーワード索引は同梱の Lindera と IPADIC で分かち書きします。分かち書きした語を BM25 で順位付けし、問い合わせと同じ語列が続く箇所に加点します。「する」「いる」「ある」や「こと」のような軽い語は、問い合わせのほかの語と並ぶ箇所の加点にだけ使います。そのため「存在しない語句」のように、軽い語だけが一致する文書は返しません。問い合わせが軽い語だけのときは、それらの語で検索します。モデルを登録せずに `nest search "ポンプの点検はいつ実施しますか"` を使えます。[Lindera のライセンス](../licenses/lindera-LICENSE.txt) · [IPADIC の通知](../licenses/ipadic-NOTICE.txt)

専門用語を登録する場合は、たとえば `dictionaries/ja.csv` に UTF-8 で次の1行を書きます。見出し行は省き、`表層形,品詞,読み` の順に並べます。表層形は、文書と問い合わせの前処理後の表記に合わせます。標準の Unicode 正規化は NFKC です。

```csv
メカニカルシール,名詞,メカニカルシール
```

```yaml
index:
  modules:
    - use: index.keyword
      with:
        tokenizer: lindera
        user_dictionary: ./dictionaries/ja.csv
```

辞書を追加・編集したあとに `nest build --from index` を実行します。各ビルドは、構築時に保存した辞書を検索にも使います。稼働中の `serve` に辞書の変更を反映するには、構築してから再起動します。既存の `tokenizer: trigram` のプロジェクトも、上の設定と同じ手順で移行できます。

## ベクトル索引を切り替える

資料が多く、埋め込みを使った検索に時間がかかる場合は、厳密なベクトル索引を近似索引（hannoy）に切り替えられます。埋め込みモデルを `models` に設定し、`nest add hannoy --json` が返す設定例を使います。`index.vector` を `hannoy.index` に、`retrieval.vector` を `hannoy.search` に置き換え、両方に同じ埋め込みモデル名を指定します。キーワードとの併用では `fusion.rrf` を残します。

```sh
nest validate
nest build --from index --tag ann
nest search "手順" --build ann
nest release ann
```

`nest add` は配布物に含まれるバイナリの版と SHA-256 を検証し、プロジェクトに配置します。既存の抽出、チャンク、埋め込みを使って索引を構築できます。`nest remove hannoy` の前には現行構成から hannoy モジュールを外します。過去のビルドやタグを読み直せるよう、追加済みバイナリと過去の索引は保持します。

## 試行に上限を設定する

エージェントに検索の調整を任せるときなど、試行の回数、所要時間、モデルへのリクエスト数と送信量に上限を設けられます。`KnowledgeNestfile.yaml` に `trial_limits` を書きます。

```yaml
trial_limits:
  max_runs: 20
  max_elapsed_ms: 300000
  max_api_requests: 100
  max_api_input_bytes: 5000000
```

上限は、タグまたは候補設定を指定した構築と、ビルドまたは候補設定を指定した評価について、同じプロジェクトで累計します。失敗した試行と送信したリクエストも数えます。候補設定では上限を引き下げられますが、引き上げることはできません。上限に達すると `TRIAL_LIMIT_REACHED` を返します。`nest status --json` の `trials` で使用量と上限を確認できます。
