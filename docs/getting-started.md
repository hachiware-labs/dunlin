# 入門：資料を、AI が引ける知識にする

[English](getting-started.en.md)

この文書は、Dunlin を初めて使う人のための道案内です。まず、手元の資料を Claude などのエージェントが引けるようにします。モデルの準備は要りません。そのあと、回答する API、AI が書く Wiki、検索を数字で良くする新しいコマンドへと進みます。

作りたいものが決まっていれば、その章から読んでも構いません。どれを作るか迷うときは、README の [どんな時に、どの文書データベースを作るか](../README.md#どんな時にどの文書データベースを作るか) を先に見てください。

| 作りたいもの | 章 |
|---|---|
| エージェント用の検索（MCP） | [2](#2-エージェントに資料を引かせる)〜[4](#4-何ができたのか) |
| 出典付きで答える API（RAG） | [5](#5-アプリに答えを返すrag) |
| AI が書く Wiki、PARA のノート | [6](#6-ai-に-wiki-を書かせるwikipara) |
| 検索を数字で良くする | [7](#7-次の段階版として残し数字で測る新しいコマンド) |

## 1 用意するもの

- Dunlin のインストール。いまはソースからのビルドです。手順は [README のインストール](../README.md#インストール) にあります。`dunlin --help` が表示されれば準備完了です。
- 資料を入れたフォルダ。この文書では、リポジトリに同梱の [examples/manuals](../examples/manuals/maintenance.md) を使います。中身は、ポンプの保全手順を書いた短い Markdown です。

コマンドは、リポジトリのルートから始めます。

## 2 エージェントに資料を引かせる

```sh
dunlin init mcp ./kb --source ./examples/manuals
cd kb
dunlin build
dunlin search "ポンプの点検はいつ実施しますか"
```

検索結果に「ポンプの点検は毎月最初の営業日に行う。」と、その出典が出れば成功です。

| コマンド | 起きること |
|---|---|
| `init mcp ./kb --source ...` | `./kb` にプロジェクトを作る。設定は `Dunlinfile.yaml` に書き出される |
| `build` | 資料を読み、検索の単位（チャンク）に分け、キーワード索引を作る |
| `search` | 日本語の文で検索し、出典付きで結果を出す |

次に、エージェントとつなぎます。

```sh
dunlin serve --print-config
```

表示された `mcpServers` の項目を、MCP クライアント（Claude Desktop の `claude_desktop_config.json`、Claude Code の `.mcp.json` など）の設定に貼ります。クライアントを起動し直して、聞いてみます。

```text
あなた   ポンプの点検って、いつやるんだっけ？
Claude   毎月最初の営業日です。点検の前にポンプを止め、電源が切れていることを確かめます。
         出典: maintenance.md 3行目、5行目
```

（やり取りはイメージです。言い回しはエージェントによって変わります。）

エージェントは、Dunlin の `knowledge.search` で資料を探し、`knowledge.read` で原文を読みます。答えを書くのはエージェント自身のモデルなので、Dunlin 側にモデルは要りません。

`build` が「未対応のファイルがある」と止まったときは、一覧を確かめます。飛ばしてよければ `dunlin build --skip-unsupported` で進めます。

## 3 資料を更新する

資料を足したり直したりしたら、`dunlin build` をもう一度実行します。

- 変わった文書だけが処理されます。
- 動いている `serve` は、次の質問から新しい索引を使います。止める必要はありません。
- 何が変わるかを先に見たいときは `dunlin build --dry-run` です。

## 4 何ができたのか

`kb` の `Dunlinfile.yaml` には、Dunlin が何をどう処理するかが書かれています。

```text
資料のフォルダ → 抽出したテキスト → チャンク → 索引 → 検索
   (source)        (source)       (transform)  (index)  (search)
```

チャンクの大きさを変えたい、PDF を読みたい、といったときは、この設定を書き換えます。`dunlin validate` で実行前に確かめられ、`dunlin modules list` で使える部品が分かります。 [Dunlinfile の設定と既定値](config.md)

## 5 アプリに答えを返す（RAG）

MCP では答えを書くのはエージェントでした。RAG では、Dunlin がモデルを呼んで、出典付きの答えまで書きます。違いの詳しい説明は [README](../README.md#mcp-と-rag-は何が違うのか) にあります。

ここでは、起動済みの Ollama に `qwen3:4b` を用意します。リポジトリのルートから始めます。

```sh
dunlin init rag ./rag --source ./examples/manuals --llm-provider ollama --llm-model qwen3:4b
cd rag
dunlin build
dunlin ask "ポンプの点検はいつ実施しますか"
dunlin serve
```

`ask` は、答えに出典の番号を付け、出典ごとにファイルと行を示します。`serve` は `127.0.0.1:4318` で、答え（`/v1/responses`）、検索（`/v1/search`）、MCP（`/mcp`）を提供します。OpenAI の SDK なら、`baseURL` を `http://127.0.0.1:4318/v1` にするだけで呼べます。 [MCP と RAG](mcp.md)

## 6 AI に Wiki を書かせる（Wiki・PARA）

Wiki では、AI が資料を読み、引用付きのページを書き溜めます。PARA は同じ操作で、Projects・Areas・Resources・Archives の4分類にノートを整理します。モデルは RAG と同じものを使います。`serve` を動かしている場合は止めてから、リポジトリのルートで始めます。

```sh
dunlin init wiki ./wiki --purpose "ポンプの保全手順をまとめる" --llm-provider ollama --llm-model qwen3:4b
cd wiki
dunlin ingress ../examples/manuals
dunlin serve
```

ブラウザで `http://127.0.0.1:4319` を開くと、左に資料とページの一覧、中央に本文と出典、右に質問と操作が並びます。PARA は `init para ./para` に読み替えます。

- ページの文には、原文への引用が付きます。引用が資料と一字一句一致しない文は保存されません。
- 原本は書き換えられません。変更は作業ごとに記録され、`dunlin undo <作業ID>` で取り消せます。
- `dunlin verify` で引用とリンクを、`dunlin verify --content` でページ同士の食い違いを確かめます。
- 質問は `dunlin query`、ページの修正は `dunlin instruct`、引用の確認は `dunlin cite` です。

[Wiki・PARA の使い方](wiki.md)

## 7 次の段階：版として残し、数字で測る（新しいコマンド）

ここからは、開発中の新しいコマンドです。作ったデータを版として残し、検索がどれだけ答えを拾えているかを数字で測ります。いまはテキストと Markdown、キーワード検索に対応していて、`serve` はまだこの索引を提供しません。

### 設定を書いて、構築する

新しいコマンドは、`init` を使わず、段階ごとの並びで書いた設定ファイルで動きます。リポジトリのルートで、新しいフォルダを作ります。

```sh
mkdir store-demo
cd store-demo
mkdir docs
cp ../examples/manuals/*.md docs/
```

`Dunlinfile.yaml` を次の内容で作ります。段階（`extract`、`transform`、`index`）ごとに処理を上から並べ、最後の `tag` が結果の名前になります。

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
dunlin build
dunlin search "ポンプの点検はいつ実施しますか"
dunlin list
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
dunlin eval-data import cases.json
dunlin eval
```

```text
Recall: 1.000 (1 of 1 cases)
```

`Recall`（回収率）は、正解の引用が検索結果に入っていた質問の割合です。質問を増やし、設定を変えて測り直し、比べて選ぶ。この続きは [使いこなす](guide.md) にあります。

## 8 うまくいかないとき

失敗したコマンドに `--json` を付けると、`errors` の `code` と `message` が分かります。

| 症状 | 対処 |
|---|---|
| `dunlin` が見つからない | `npm link` を実行し、`npm prefix -g` の場所を `PATH` に加える |
| `PROJECT_REQUIRED` | `Dunlinfile.yaml` のあるディレクトリで実行するか、`--project <directory>` を付ける |
| `SOURCE_MISSING` | `--source` や `source.filesystem` の `path` が、あるフォルダを指しているか確かめる |
| `MODEL_REQUIRED`、モデルにつながらない | Ollama が起動しているか、モデルを取得済みかを確かめる |
| `... is not on the new store yet.` | その部品は新しいコマンドではまだ使えない。`init` が作る形の設定で使う（[付録](#付録-2つの設定の形)） |
| `No loader takes N files` | 新しいコマンドが読むのは `.md`、`.markdown`、`.txt` だけ |

そのほかは [困ったときは](troubleshooting.md) にあります。

## 9 次に読むもの

- 検索を数字で良くする：[使いこなす](guide.md)
- 4種別をサンプルで順に試す：[クイックスタート](quickstart.md)
- コマンドの一覧：[CLI リファレンス](cli.md)

## 付録 2つの設定の形

検索（`mcp`・`rag`）のコマンドは、読む設定ファイルの形で、どの仕組みを使うかを選びます。

| 設定ファイルの形 | 例 | 仕組み | 使えるコマンド |
|---|---|---|---|
| `modules:` と `use:` を使う形 | `dunlin init mcp`・`init rag` が作るもの（2〜5章） | これまでのビルド | `build`、`search`、`eval`、`eval-data`、`source`、`transform`、`index`、`status`、`use`、`tag`、`serve`、`ask` |
| 段階ごとの並び（`extract:` の下に `- source.filesystem: ...`） | 7章の設定 | 版の保管（`.dunlin/store`） | `build`、`search`、`eval`、`eval-data import`、`list`、`results`、`optimize`、`release` |

| できること | `init` が作る形 | 段階ごとの並び |
|---|---|---|
| 読める資料 | Markdown、テキスト、HTML、PDF、Word、Excel、PowerPoint、OCR | `.md`、`.markdown`、`.txt` |
| 検索 | キーワード、ベクトル、ハイブリッド、リランク、ユーザー辞書 | キーワード |
| 版の記録と再利用、候補の一括比較 | — | ○ |
| `serve`（MCP・HTTP）、`ask` | ○ | まだ |

- 段階ごとの並びでは、`build` の `--from`、`--until`、`--force`、`--input`、`--dry-run`、`--skip-unsupported`、`--tag`、`eval` の `--build`、`--split`、`--tag` はまだ使えません。
- 1つのフォルダに、両方の形の設定ファイルを置いても構いません。
- Wiki と PARA は、どちらの形とも別の仕組みです。
