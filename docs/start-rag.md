# RAG 入門：アプリに、出典付きで答える API を作る

[English](start-rag.en.md) · [入門の一覧](getting-started.md)

自分のアプリやチャットボットから質問を送ると、資料に基づく答えが出典付きで返る API を作ります。応答は OpenAI の Responses API の形なので、OpenAI の SDK がそのまま使えます。

## こんなときに

- 社内ポータルやチャットボットに「資料に基づく Q&A」を付けたい
- 問い合わせへの回答に、根拠を必ず添えたい
- エージェントではなく、自分のコードから答えを受け取りたい

## しくみ

```text
アプリ ─質問→ Knowledge-Nest ─検索→ 根拠 ─→ 回答モデル ─→ 出典付きの答え ─→ アプリ
```

MCP との違いは、答えを書くのが Knowledge-Nest の呼ぶモデルだという点です。アプリは質問を1回送り、答えを1回受け取るだけです。

答えの質は、根拠を拾えているかで決まります。答えの土台になる検索は、MCP と同じように、資料の更新、当たり具合の測定、ユーザー辞書、構成の見直しで育てられます（[MCP 入門](start-mcp.md) の4章）。答えを書くモデルは、設定を変えて `serve` を起動し直すだけで差し替えられます。

## ほかの方法と比べて

- **LangChain などで組むより。** コードは書きません。設定ファイルとコマンドで立ち上がります。答えの API は OpenAI の SDK がそのまま使える形なので、既存のアプリは接続先を変えるだけで呼べます。
- **根拠がなければ、答えない。** 資料に根拠が見つからない質問には、答えを作らずに断ります。答えの出典は、資料の版と原文の範囲まで指します。
- **ノーコードの RAG アプリより。** 資料も索引も手元のフォルダに置いたまま動きます。モデルを手元の Ollama にすれば、資料は外に出ません。ほかのマシンに開くときは、認証と接続元の制限を付けられます。
- **答えの土台を、測って最適化できる。** 答えの質は、根拠を拾えているかで決まります。検索は MCP と同じなので、回収率で測り、設定を最適化できます。

## 1 回答モデルを用意する

ここでは、手元で動く Ollama の `qwen3:4b` を使います。Ollama を起動し、`ollama pull qwen3:4b` でモデルを取得しておきます。

```sh
nest config set llm --provider ollama --model qwen3:4b
```

一度登録すると、以後に作る RAG・Wiki・PARA のプロジェクトがこのモデルを使います。OpenAI などの API を使う場合は `--provider openai` とし、API キーを環境変数 `OPENAI_API_KEY` に入れます。設定ファイルにキーは書きません。

## 2 プロジェクトを作って、質問する

```sh
nest init rag ./rag --source ./manuals
cd rag
nest build
nest ask "ポンプの点検はいつ実施しますか"
```

`ask` は、答えに `[1]` のような出典の番号を付け、番号ごとにファイル、行、見出しを示します。検索だけを確かめたいときは `nest search "..."` です。

## 3 API として提供する

```sh
nest serve
```

`127.0.0.1:4318` の1つのポートで、3つの入口を提供します。

| 入口 | 内容 |
|---|---|
| `POST /v1/responses` | 質問を送ると、出典付きの答えが返る（OpenAI Responses API の形） |
| `POST /v1/search` | 検索結果と、組み立てた文脈だけを返す |
| `/mcp` | エージェント向けの MCP。答えは書かず、根拠を返す |

### curl で呼ぶ

```sh
curl http://127.0.0.1:4318/v1/responses -H "Content-Type: application/json" -d '{"input":"メカニカルシールの交換周期は？"}'
```

Windows の `curl` で日本語を直接書くと UTF-8 にならないことがあります。そのときは JSON を UTF-8 のファイルに保存し、`--data-binary @request.json` で送ります。

### OpenAI の SDK で呼ぶ

```python
from openai import OpenAI

client = OpenAI(base_url="http://127.0.0.1:4318/v1", api_key="unused")
response = client.responses.create(model="qwen3:4b", input="メカニカルシールの交換周期は？")
print(response.output_text)
```

`model` には、プロジェクトに設定したモデル名を渡します。違う名前を渡すと `MODEL_MISMATCH` になります。答えの引用は `output[].content[].annotations` に、引用した資料の版と原文の範囲は `knowledge_nest_citations` に入ります。

## 4 資料を更新する

資料を足したり直したりしたら `nest build` です。変わった文書だけが処理され、動いている `serve` は次の要求から新しい索引を使います。

## 5 外に公開する

`serve` は、標準では自分のマシン（`127.0.0.1`）からの接続だけを受けます。ほかのマシンから使わせるときは、`KnowledgeNestfile.yaml` の `serve.access` を設定します。

- `auth.type: token` と `token_env`（トークンを入れる環境変数の名前）。クライアントは `Authorization: Bearer <token>` を送る
- `allowed_hosts` と `allowed_origins`（受け付ける宛先とブラウザの送信元）
- `serve.limits`（本文の大きさ、時間、同時に受ける数）

[HTTP の接続制限](mcp.md#http-の接続制限)

## 6 うまくいかないとき

| 症状 | 対処 |
|---|---|
| `/v1/responses` が `503 MODEL_REQUIRED` | 回答モデルが未設定。`models.answerer.model` を設定して `serve` を起動し直す。検索（`/v1/search`）と MCP はモデルなしでも動く |
| モデルにつながらない | Ollama が起動しているか、モデルを取得済みかを確かめる。API なら `api_key_env` の環境変数を確かめる |
| `MODEL_MISMATCH` | `model` を省くか、プロジェクトのモデル名にする |
| `INVALID_JSON` | 本文を UTF-8 の JSON で送る |
| ポートが使用中 | `nest serve --port 4320` のように空いているポートにする |

そのほかは [困ったときは](troubleshooting.md) にあります。

## 次に読むもの

- 答えの質を数字で良くする：[検索を数字で良くする](guide.md) · [検索評価とユーザー辞書](evaluation.md)
- エージェントに直接引かせる：[MCP 入門](start-mcp.md)
- 応答の形と設定の詳細：[MCP と RAG](mcp.md)
