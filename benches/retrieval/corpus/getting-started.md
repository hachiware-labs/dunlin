# 入門：資料を、AI が引ける知識にする

[English](getting-started.en.md)

Knowledge-Nest を初めて使う人のための入口です。作りたいものを1つ選び、その入門に進んでください。どれも、手元の資料フォルダ1つから始められます。

## 1 どれから始めるか

| 作りたいもの | こんなときに | モデル | 入門 |
|---|---|---|---|
| **MCP**：エージェント用の検索 | Claude などに、資料を引いて答えさせたい | 要らない | [MCP 入門](start-mcp.md) |
| **RAG**：出典付きで答える API | 自分のアプリやチャットボットで、答えを受け取りたい | 回答に要る | [RAG 入門](start-rag.md) |
| **Wiki**：AI が書く知識のページ | 資料を、人もエージェントも読めるページにまとめたい | 執筆に要る | [Wiki 入門](start-wiki.md) |
| **PARA**：AI が整理するノート | 溜まった資料やメモを、仕事の進め方に沿って整理したい | 執筆に要る | [PARA 入門](start-para.md) |

- **迷ったら MCP から。** モデルの準備が要らず、数分で試せます。
- **検索型と蓄積型。** MCP と RAG は、質問のたびに資料を探します。Wiki と PARA は、AI が先に資料を読んで、ページやノートにまとめておきます。
- **どれも出典付き。** 検索結果も、答えも、ページの文も、元の資料の場所を指します。

もっと詳しい選び方は、README の [どんな時に、どの文書データベースを作るか](../README.md#どんな時にどの文書データベースを作るか) にあります。

## 2 用意するもの

- Knowledge-Nest のインストール。いまはソースからのビルドです。手順は [README のインストール](../README.md#インストール) にあります。`nest --help` が表示されれば準備完了です。
- 資料を入れたフォルダ。手元にない場合は、リポジトリに同梱の [examples/manuals](../examples/manuals/maintenance.md) を使えます。ポンプの保全手順を書いた短い Markdown です。
- RAG、Wiki、PARA では、モデルが要ります。入門では、手元で動く Ollama の `qwen3:4b` を使います。

4つの形をサンプル資料で一度に試すなら [クイックスタート](quickstart.md) もあります。

## 3 検索を数字で良くする（新しいコマンド）

MCP と RAG の検索を、数字で測って良くするための、開発中の新しいコマンドです。作ったデータを版として残し、検索がどれだけ答えを拾えているかを数字で測ります。いまはテキストと Markdown、キーワード検索に対応していて、`serve` はまだこの索引を提供しません。

### 設定を書いて、構築する

新しいコマンドは、`init` を使わず、段階ごとの並びで書いた設定ファイルで動きます。リポジトリのルートで、新しいフォルダを作ります。

```sh
mkdir store-demo
cd store-demo
mkdir docs
cp ../examples/manuals/*.md docs/
```

`KnowledgeNestfile.yaml` を次の内容で作ります。段階（`extract`、`transform`、`index`）ごとに処理を上から並べ、最後の `tag` が結果の名前になります。

```yaml
extract:
  - source.filesystem: { path: ./docs }
  - loader.text: {}
  - tag: docs

transform:
  - chunk.length: { unit: characters, size: 800, overlap: 100 }
  - tag: chunks

index:
  - index.keyword: { tokenizer: lindera }
  - tag: main

search:
  - retrieval.keyword: { top_k: 10 }
```

```sh
nest build
nest search "ポンプの点検はいつ実施しますか"
nest list
```

`build` は、動いた段階と文書の増減を表示します。もう一度 `build` すると、何も変わっていなければ `Nothing ran: every result was there already.` と出て、何も作り直しません。`list` は、名前の付いた版（文書、チャンク、索引、評価データ、評価の結果）を一覧します。

### 測る

質問と、その答えになる引用の組（評価データ）を `cases.json` に書きます。

```json
[
  {
    "id": "pump-check",
    "question": "ポンプの点検はいつ",
    "source": "docs",
    "document": "maintenance.md",
    "quote": "ポンプの点検は毎月最初の営業日に行う。",
    "split": "dev"
  }
]
```

| 項目 | 意味 |
|---|---|
| `source` | 資料のフォルダの名前（`source.filesystem` の `id`。省略時はフォルダ名） |
| `document` | そのフォルダの中でのファイルの位置 |
| `quote` | 文書に1か所だけ現れる、答えの引用 |
| `split` | 調整用は `dev`、最終確認用は `test` |

設定の最後に評価の区切りを足して、測ります。

```yaml
eval:
  - eval.evidence: { data: default, split: dev }
```

```sh
nest eval-data import cases.json
nest eval
```

```text
Recall: 1.000 (1 of 1 cases)
```

`Recall`（回収率）は、正解の引用が検索結果に入っていた質問の割合です。質問を増やし、設定を変えて測り直し、比べて選ぶ。この続きは [使いこなす](guide.md) にあります。

## 4 次に読むもの

- 検索を数字で良くする：[使いこなす](guide.md)
- コマンドの一覧：[CLI リファレンス](cli.md)
- 困ったとき：[困ったときは](troubleshooting.md)

## 付録 2つの設定の形

検索（`mcp`・`rag`）のコマンドは、読む設定ファイルの形で、どの仕組みを使うかを選びます。

| 設定ファイルの形 | 例 | 仕組み | 使えるコマンド |
|---|---|---|---|
| `modules:` と `use:` を使う形 | `nest init mcp`・`init rag` が作るもの（MCP 入門、RAG 入門） | これまでのビルド | `build`、`search`、`eval`、`eval-data`、`source`、`transform`、`index`、`status`、`use`、`tag`、`serve`、`ask` |
| 段階ごとの並び（`extract:` の下に `- source.filesystem: ...`） | 3章の設定 | 版の保管（`.knowledge-nest/store`） | `build`、`search`、`eval`、`eval-data import`、`list`、`results`、`optimize`、`release` |

| できること | `init` が作る形 | 段階ごとの並び |
|---|---|---|
| 読める資料 | Markdown、テキスト、HTML、PDF、Word、Excel、PowerPoint、OCR | `.md`、`.markdown`、`.txt` |
| 検索 | キーワード、ベクトル、ハイブリッド、リランク、ユーザー辞書 | キーワード |
| 版の記録と再利用、候補の一括比較 | — | ○ |
| `serve`（MCP・HTTP）、`ask` | ○ | まだ |

- 段階ごとの並びでは、`build` の `--from`、`--until`、`--force`、`--input`、`--dry-run`、`--skip-unsupported`、`--tag`、`eval` の `--build`、`--split`、`--tag` はまだ使えません。
- 1つのフォルダに、両方の形の設定ファイルを置いても構いません。
- Wiki と PARA は、どちらの形とも別の仕組みです。
