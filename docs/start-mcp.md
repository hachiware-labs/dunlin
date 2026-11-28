# MCP 入門：エージェントに、あなたの資料を引かせる

[English](start-mcp.en.md) · [入門の一覧](getting-started.md)

Claude Desktop や Claude Code などのエージェントが、手元の資料を検索し、出典を示して答えられるようにします。Knowledge-Nest 側にモデルは要りません。

## こんなときに

- エージェントに作業させながら、社内の手順書や設計書を調べさせたい
- 資料を外部のモデルに送らず、まずは手元だけで試したい
- エージェントが言い換えながら何度も資料を引けるようにしたい

## しくみ

```text
あなた ─質問→ エージェント ─knowledge.search→ Knowledge-Nest ─→ 該当箇所と出典
                  │                                    │
                  └←─────────── knowledge.read で原文を読む ─┘
                  └─ 読んだ根拠から、エージェント自身のモデルが答える
```

Knowledge-Nest が返すのは「答え」ではなく「根拠」です。答えの文章を書くのは、エージェント自身のモデルです。

検索は、入れて終わりではなく育てていくものです。資料を直せば `nest build` で変わった文書だけが入れ替わり、動いているエージェントは止めずに新しい内容を引きます。質問と正解の引用で当たり具合を測り、ユーザー辞書や構成の見直しで当たるようにしていけます（4章）。試しても提供中の検索は変わらず、確かめた結果だけを `nest release` で切り替えます。

## ほかの方法と比べて

- **エージェントにフォルダを直接読ませるより。** PDF・Word・Excel・スキャンした資料も、日本語の語で引けます。返すのは答えのある箇所だけで、資料名と行が付き、決めた文字数に収まります。資料が何千件あっても、エージェントの文脈を埋めません。
- **ベクトル検索だけの検索サーバーより。** 型番、条番号、専門用語のように「その語で探したい」検索に強い日本語のキーワード検索が、埋め込みモデルなしで動きます。言い換えに強いベクトル検索を足して、ハイブリッドにもできます。
- **測って、最適化まで。** 質問と正解の引用を登録すると（質問の候補は AI にも作らせられます）、検索が根拠を拾えた割合を測れます。チャンクの大きさや検索の件数などの候補をまとめて試し、良い順に並べ、確かめた設定だけを提供に切り替えます。自分で組んだ検索では、この評価と調整の仕組みを別に用意することになります。 [検索を、測って最適化する](guide.md)

## 1 プロジェクトを作って検索する

資料のフォルダを `./manuals` とします。手元にない場合は、リポジトリの `examples/manuals` を使えます。

```sh
nest init mcp ./kb --source ./manuals
cd kb
nest build
nest search "ポンプの点検はいつ実施しますか"
```

| コマンド | 起きること |
|---|---|
| `init mcp` | プロジェクトを作り、処理の設定を `KnowledgeNestfile.yaml` に書き出す |
| `build` | 資料を読み、チャンク（検索の単位）に分け、キーワード索引を作る |
| `search` | 日本語の文で検索し、資料名・行・見出しの付いた結果を出す |

`build` が未対応のファイルで止まったら、一覧を確かめて `nest build --skip-unsupported` で飛ばせます。

## 2 エージェントにつなぐ

```sh
nest serve --print-config
```

表示された `mcpServers` の項目を、MCP クライアントの設定に貼ります。

| クライアント | 設定ファイルの例 |
|---|---|
| Claude Desktop | `claude_desktop_config.json` |
| Claude Code | プロジェクトの `.mcp.json` |

クライアントを起動し直すと、エージェントが Knowledge-Nest を起動して使い始めます。聞いてみましょう。

```text
あなた   ポンプの点検って、いつやるんだっけ？
Claude   毎月最初の営業日です。点検の前にポンプを止め、電源が切れていることを確かめます。
         出典: maintenance.md 3行目、5行目
```

（やり取りはイメージです。）

エージェントが使える道具は次の2つです。

| 道具 | 内容 |
|---|---|
| `knowledge.search` | 資料を検索し、見つけた箇所の一覧と、上位の箇所の本文を返す。件数（`limit`）、本文の文字数（`budget_chars`）、対象の資料（`source_ids`）を指定できる |
| `knowledge.read` | 検索で見つけた箇所を、行番号付きで読む。前後の行（`around`）や、文書の好きな行も読める |

検索の結果は、エージェントが読みやすく、文脈を食わない短いテキストです。

```text
3 passages for "ポンプの点検". Read more of one with knowledge.read and its ref.
1. docs:maintenance.md#L3-5 — ポンプの保全手順
2. ...

Text of the best passages:

maintenance.md — ポンプの保全手順
file:///.../maintenance.md:3
ポンプの点検は毎月最初の営業日に行う。
...
```

- `docs:maintenance.md#L3-5` のような **ref** が、資料と行です。エージェントはこれを出典として示し、`knowledge.read` に渡して続きを読みます。`docs:maintenance.md#L1-200` のように好きな行を、`docs:maintenance.md` で文書全体を読めます。読み出しは既定で 20000 文字までで、収まらないときは続きの ref を示します。
- 道具の説明には、資料の数と、どの資料のフォルダに入っているかが書かれます。エージェントは、これを見て道具を使う場面を決めます。
- 得点や版、出典の範囲まで欲しいときは、`knowledge.search` に `format: "json"` を渡します。`nest search --json` と同じ形の結果が返ります。

## 3 資料を更新する

資料を足したり直したりしたら、`nest build` をもう一度実行します。

- 変わった文書だけが処理されます。
- 動いている `serve` は、次の要求から新しい索引を使います。エージェントを止める必要はありません。
- 何が変わるかを先に見るには `nest build --dry-run` です。

## 4 検索を自分の資料に合わせる

`KnowledgeNestfile.yaml` を書き換えて `build` し直します。設定は `nest validate` で実行前に確かめられます。

| やりたいこと | すること |
|---|---|
| 専門用語が分かれて引けない | ユーザー辞書を足す（`index.keyword` の `user_dictionary`）。候補は `nest dictionary suggest` が出す |
| 答えが長いチャンクに埋もれる | `chunk.length` の `size` を小さくする。見出しのある文書は `chunk.structure` |
| 言い換えた質問に弱い | 埋め込みモデルを足してハイブリッド検索にする（`--embedding-model`） |
| PDF、Word、Excel、PowerPoint、HTML を読む | 標準の `loader.auto` が読む。スキャンした PDF や画像は `nest add ocr` で OCR を足す |
| 読ませたくないファイルがある | `source.filesystem` の `exclude` に書く |

変えた結果が良くなったかは、勘ではなく数字で確かめられます。[検索を数字で良くする](guide.md) · [検索評価とユーザー辞書](evaluation.md)

## 5 うまくいかないとき

| 症状 | 対処 |
|---|---|
| クライアントに道具が出てこない | 貼った設定の `command` と `args` を確かめ、クライアントを起動し直す。`nest` が `PATH` にあるかも確かめる |
| `BUILD_REQUIRED` | 先に `nest build` を実行する |
| 検索結果が期待と違う | `nest search "質問" --json` で、エージェントが受け取る結果を直接見る |
| `UNSUPPORTED_FILES` | `exclude` で除くか、`build --skip-unsupported` で飛ばす |

そのほかは [困ったときは](troubleshooting.md) にあります。

## 次に読むもの

- 自分のアプリに答えを返したい：[RAG 入門](start-rag.md)
- 人が読むページとしてもまとめたい：[Wiki 入門](start-wiki.md)
- 設定と提供の詳細：[MCP と RAG](mcp.md)
