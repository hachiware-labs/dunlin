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
| `knowledge.search` | 資料を検索し、該当箇所と出典、文脈を返す。件数（`limit`）、文脈の文字数（`budget_chars`）、対象の資料（`source_ids`）を指定できる |
| `knowledge.read` | 検索で見つけた箇所を、原文の行として読む |

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
