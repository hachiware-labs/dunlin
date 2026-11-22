# Dunlin

[English](README.en.md)

**資料のフォルダを渡すだけで、AI が「根拠を示して」答えられる知識になる。**

Dunlin は、手元の資料から AI のための文書データベースを作るツールです。同じ資料から、エージェントが引く検索（MCP）、出典付きで答える API（RAG）、AI が書き育てる Wiki（LLM-Wiki）や整理されたノート（PARA）を作れます。資料も索引も手元に置いたまま、答えにはいつも出典が付きます。

```text
資料フォルダ ──→  Dunlin  ──┬─ MCP    Claude などのエージェントが、資料を引いて答える
(Markdown、PDF、             ├─ RAG    自分のアプリが、API で出典付きの答えを受け取る
 Word、Excel …)             ├─ Wiki   AI が資料を読み、引用付きのページを書き溜める
                            └─ PARA   AI が資料やメモを、PARA の4分類に整理する
```

## こんなことができます

以下は使い方のイメージです。資料は [ポンプの保全手順](examples/manuals/maintenance.md) です。

**Claude に社内マニュアルを引かせる（MCP）**

```text
あなた   ポンプの点検って、いつやるんだっけ？
Claude   毎月最初の営業日です。点検の前にポンプを止め、電源が切れていることを確かめます。
         出典: maintenance.md 3行目、5行目
```

**自分のアプリから、出典付きの答えを受け取る（RAG）**

```sh
curl http://127.0.0.1:4318/v1/responses -d '{"input":"メカニカルシールの交換周期は？"}'
# → 「2年ごとです [1]」と、[1] が maintenance.md の7行目を指す引用
```

**AI に Wiki を書かせる。ただし、根拠のない文は1行も残さない（Wiki）**

```text
ポンプの保全
  点検は毎月最初の営業日に行う [1]。点検前に停止と電源断を確かめる [2]。
  [1] maintenance.md 3行目  [2] maintenance.md 5行目
```

## 3分で試す：エージェントに資料を引かせる

モデルの準備は要りません。日本語のキーワード検索が、手元ですぐに動きます。

```sh
dunlin init mcp ./kb --source ./manuals
cd kb
dunlin build
dunlin search "ポンプの点検はいつ実施しますか"
dunlin serve --print-config
```

最後のコマンドが出す設定を、Claude Desktop や Claude Code などの MCP クライアントの設定に貼れば、エージェントがあなたの資料を引けるようになります。資料を直したら `dunlin build` をもう一度。変わった文書だけが処理されます。

インストールは [こちら](#インストール)。順を追った説明は [入門](docs/getting-started.md) にあります。

## どんな時に、どの文書データベースを作るか

### 2つの問いで決める

```text
Q1 その知識を、主に読むのは誰？
   │
   ├─ AI（エージェントやアプリ）が、質問のたびに引く
   │     │
   │     Q2 答えの文章を書くのは誰？
   │     ├─ エージェント自身（Claude など） ………………… MCP
   │     └─ Dunlin（アプリは答えを受け取るだけ） …… RAG
   │
   └─ 人が、ページとして読み、育てていく
         │
         Q2 どう並べたい？
         ├─ テーマごとの知識のページに …………………… Wiki
         └─ 仕事の進め方（PARA の4分類）に沿って ……… PARA
```

### 場面から選ぶ

| こんな時 | 作るもの | 理由 |
|---|---|---|
| Claude Code で作業しながら、社内の手順書や設計書を調べさせたい | **MCP** | エージェントが自分で言い換えて何度も引ける。モデルの準備が要らない |
| 資料を社外のモデルに渡したくない。まずは手元だけで試したい | **MCP** | キーワード検索は手元で完結する。答えるのは、すでに使っているエージェント |
| 社内ポータルやチャットボットに「資料に基づく Q&A」を付けたい | **RAG** | HTTP で質問すると、出典付きの答えが返る。OpenAI の SDK がそのまま使える |
| 問い合わせ対応で、回答の根拠を必ず示したい | **RAG** | 答えの引用が、資料の版と原文の範囲まで指す |
| 散らばった手順書や議事録から、新人が読めるナレッジベースを作りたい | **Wiki** | AI がテーマごとのページにまとめ、すべての文に引用を付ける |
| 規程やマニュアルの間の食い違い、古くなった記述を見つけたい | **Wiki** | `verify --content` がページ同士の矛盾を、`verify` が古い引用を探す |
| 調べもののメモや資料が溜まり、どこに何があるか分からない | **PARA** | AI が、案件・継続業務・参考資料・完了済みに振り分ける |
| 案件ごとの資料と、続けて担当する業務の資料を分けて管理したい | **PARA** | Projects と Areas を分けて持てる。終わった案件は Archives へ |
| 人も読み、エージェントも引けるようにしたい | **Wiki と MCP** | 同じ資料フォルダを、`wiki` と `mcp` の2つのプロジェクトに渡す |
| 検索の精度を、根拠を持って説明・改善したい | **MCP か RAG** | 質問と正解の組で回収率を測り、設定を比べて選べる |

### 4つの形の早見表

| | MCP | RAG | Wiki | PARA |
|---|---|---|---|---|
| できるもの | エージェント用の検索 | 出典付きで答える API | 引用付きの知識のページ | PARA 形式のノート |
| 主に使うのは | エージェント | 自分のアプリ | 人（エージェントは `query` で） | 人（エージェントは `query` で） |
| 資料を読むとき | 質問のたび | 質問のたび | 取り込むとき | 取り込むとき |
| モデル | 要らない | 回答に要る | 執筆に要る | 執筆に要る |
| 資料の更新 | `build` で、変わった文書だけ | `build` で、変わった文書だけ | `ingress` で、ページを書き足す | `ingress` で、ノートを書き足す |
| 提供のしかた | MCP（stdio） | HTTP（回答・検索・MCP） | ブラウザの画面 | ブラウザの画面 |

- **モデルの手間**: MCP だけは、Dunlin 側にモデルが要りません。RAG は質問のたびに、Wiki と PARA は取り込むたびにモデルを呼びます。
- **迷ったら MCP から。** 資料フォルダ1つで、すぐに試せます。あとから同じ資料で RAG や Wiki を作っても、MCP はそのまま使えます。

### MCP と RAG は何が違うのか

どちらも「質問のたびに資料を検索する」形です。違うのは、**見つけた根拠から答えを書くのが誰か**です。

```text
MCP   質問 ─→ [エージェント] ─検索を依頼─→ Dunlin ─→ 根拠（資料の該当箇所）
                  ↑                                      │
                  └──────── 根拠を読んで、エージェントのモデルが答える ──┘

RAG   質問 ─→ Dunlin ─→ 検索 ─→ 根拠 ─→ Dunlin が呼ぶモデルが答えを書く ─→ 出典付きの回答
```

| | MCP | RAG |
|---|---|---|
| たとえると | 司書が棚から資料を出して、調べる人に渡す | 司書が資料を読んで、答えを書いて渡す |
| Dunlin が返すもの | 根拠（資料の該当箇所と出典） | 出典付きの回答 |
| 答えを書くのは | エージェント側のモデル（Claude など） | Dunlin が呼ぶモデル（Ollama や API） |
| 呼び出す側 | MCP に対応したエージェント・アプリ | HTTP で呼べるもの（自作アプリ、OpenAI の SDK など） |
| Dunlin 側のモデル | 要らない | 回答モデルが要る（未設定でも検索は使える） |
| 質問の仕方 | エージェントが、必要に応じて言い換えて何度も引く | 質問を1回送ると、答えが1回返る |

- **Claude などに資料を調べさせたい** → MCP。
- **自分のアプリで答えを受け取りたい** → RAG。
- **迷ったら** → MCP から。モデルが要りません。RAG の `serve` は MCP（`/mcp`）も同じポートで出すので、あとから RAG に移っても、エージェントからの検索はそのまま使えます。

### MCP：Claude に、あなたの資料を引かせる

- **準備はフォルダ1つ。** GPU も埋め込みモデルも要らず、日本語の自然文でそのまま引けます。資料は手元から出ません。
- **答えに、行まで付いた出典。** 結果には資料名・行・見出しが付き、エージェントは `knowledge.read` で原文まで読み戻せます。
- **つなぐのは貼るだけ。** `dunlin serve --print-config` の出力を、MCP クライアントの設定に貼ります。
- **資料の更新に、止めずについていく。** `dunlin build` を再実行すると、変わった文書だけを処理し、動いている検索は次の質問から新しい索引を使います。

日本語は Lindera と IPADIC で分かち書きします。「する」「こと」のような軽い語だけが合う文書は返しません。専門用語はユーザー辞書で足せます。ベクトルを足したハイブリッド検索、PDF・Office・HTML・OCR にも対応します。 [MCP と RAG](docs/mcp.md)

### RAG：アプリに、出典付きで答える API

- **OpenAI の SDK が、そのまま使える。** 応答は OpenAI の Responses API の形です。`baseURL` を `http://127.0.0.1:4318/v1` に向けるだけです。
- **答えの引用が、原文の範囲まで指す。** 引用には、資料の版と、原文の中の範囲が付きます。
- **1つのポートで、答え・検索・MCP。** `/v1/responses`、`/v1/search`、`/mcp` を同時に提供します。
- **モデルは手元でも API でも。** ローカルの Ollama も、外部の API も選べます。外に公開するときは、トークン認証と接続制限を設定できます。

```sh
dunlin config set llm --provider ollama --model qwen3:4b
dunlin init rag ./rag --source ./manuals
cd rag
dunlin build
dunlin ask "ポンプの点検周期は？"
dunlin serve
```

[MCP と RAG](docs/mcp.md)

### Wiki：AI が書き、根拠のない文は残らない

- **知識が、ページとして育つ。** 質問のたびに資料を引き直す代わりに、AI が資料を読んで、整ったページに書き溜めます。資料を足すたびにページが育ちます。
- **AI の書いた文を、1文ずつ検証。** 記述には原文の引用が付き、引用が資料と一字一句一致しない文は保存されません。
- **何をしても戻せる。** 資料の原本は書き換えません。変更は作業ごとに記録され、`dunlin undo` で取り消せます。
- **古くなった所を見つける。** `dunlin verify` がリンク切れや古い引用を、`--content` を付けるとページ同士の食い違いも見つけます。
- **ブラウザで読み、頼む。** 画面で資料・ページ・出典を並べて読み、「点検前の確認事項を箇条書きに」のようにページへ指示できます。

```sh
dunlin config set llm --provider ollama --model qwen3:4b
dunlin init wiki ./wiki --purpose "設備保全の手順と部品情報をまとめる"
cd wiki
dunlin ingress ../manuals
dunlin serve        # http://127.0.0.1:4319 をブラウザで開く
```

エージェントからは `dunlin query "質問" --json` でページに基づく答えを、プロジェクトの `site/` から HTML のページを読めます。Wiki の `serve` は人が使うブラウザの画面で、MCP のサーバーではありません。エージェントが MCP で引く形が欲しいときは、同じ資料で `mcp` のプロジェクトも作ります。 [Wiki・PARA の使い方](docs/wiki.md)

### PARA：AI が、資料とメモを整理する

PARA は、情報を「いま何に使うか」で4つに分ける整理法です。

| 分類 | 入るもの | 例 |
|---|---|---|
| Projects | 期限のある取り組み | 来月のポンプ更新工事 |
| Areas | 続けて責任を持つ領域 | 設備の定期保全 |
| Resources | いつか役立つ参考資料 | 部品メーカーのカタログ |
| Archives | 終わったもの | 昨年度の点検記録 |

- **振り分けを AI に任せられる。** 資料を取り込むと、AI がノートを書いて分類に置きます。
- **Wiki と同じ安心。** 引用の検証、原本を書き換えない保管、作業の取り消しは Wiki と共通です。

`init wiki` を `init para ./para` に読み替えるだけで始められます。

## Dunlin を選ぶ理由

- **手元で完結する。** 資料も索引もプロジェクトのフォルダに保存されます。外へ送るのは、あなたがモデルを設定したときに、その処理に要るテキストだけです。
- **いつも出典が付く。** 検索結果も、答えも、Wiki の文も、元の資料の場所を指します。
- **勘ではなく、数字で良くする。** 「この質問なら、この文が答え」という組を登録すると、検索が根拠を拾えている割合（回収率）を測れます。設定を変えて測り直し、比べてから選べます（MCP・RAG）。
- **試すのが速い。** 同じ入力と同じ処理は2度実行しません。チャンクの大きさだけ変えた試しなら、資料の読み直しは起きません。だから何通りも試せます。
- **構成が見える。** 何をどう処理するかは `Dunlinfile.yaml` に書き出され、`dunlin validate` で実行前に確かめられます。

LangChain やベクトルデータベースとの比べ方は [Dunlin が向いている構成](docs/comparison.md) にあります。

## 検索を数字で良くする（開発中の新しいコマンド）

検索結果やデータを版として残し、設定の候補をまとめて測って比べる仕組みを作っています。

```sh
dunlin eval-data import cases.json   # 質問と、答えになる引用の組を入れる
dunlin eval                          # いまの設定で回収率を測る
dunlin optimize tune.yaml            # 変えたい所を書いた指示から、候補を作って順に測る
dunlin results tune                  # 候補を、回収率の高い順に並べる
dunlin release tune/010              # 選んだ索引に、提供用の名前を付ける
```

`results` の表示の例です（一部の列を省いています）。

```text
Trial  Recall  To base  What
010    1.000   +0.333   chunking=length(size=200),search=top10
000    0.667   -        base
```

いまはテキストと Markdown、キーワード検索に対応しています。`serve` はまだこの仕組みの索引を提供しません。手順と、対応範囲は [使いこなす](docs/guide.md) にあります。

## インストール

npm への公開は準備中です。現在はソースからビルドします。Rust 1.95 以降と、Node.js 22.20 以降の 22 系または 24.12 以降の 24 系が必要です。

```sh
git clone https://github.com/hachiware-labs/dunlin
cd dunlin
npm ci
npm run build
npm link
dunlin --help
```

## ドキュメント

| 目的 | 文書 |
| --- | --- |
| 初めて使う | [入門](docs/getting-started.md) · [クイックスタート](docs/quickstart.md) |
| 使いこなす | [使いこなす](docs/guide.md) |
| 検索・回答・ページを使う | [MCP と RAG](docs/mcp.md) · [Wiki と PARA](docs/wiki.md) |
| 検索を改善する | [検索評価とユーザー辞書](docs/evaluation.md) · [エージェントと試行する](docs/agents.md) · [精度を上げる手法の候補](docs/retrieval-techniques.md) |
| コマンドや設定を調べる | [CLI](docs/cli.md) · [モジュール](docs/modules.md) · [Dunlinfile の既定値](docs/config.md) · [困ったときは](docs/troubleshooting.md) |
| 仕組みと用途を知る | [向いている構成](docs/comparison.md) · [データ管理システム](docs/data-management.md) · [ビルドと評価の関係](docs/concepts.md) · [実装構成](docs/architecture.md) · [ベンチマーク](docs/benchmarks.md) · [PDF の抽出](docs/pdf-extraction.md) |
| Node.js に組み込む | [Node.js API](docs/api.md) |
| 開発・運用を確認する | [開発と変更の確認](CONTRIBUTING.md) · [変更履歴](CHANGELOG.md) · [セキュリティ](SECURITY.md) · [受け入れ確認](docs/acceptance.md) |
| ライセンスと通知を調べる | [本体のライセンス状況](LICENSE) · [第三者の通知](THIRD_PARTY_NOTICES.md) |
| 仕様を調べる | [Dunlin 設計仕様](docs/dunlin-spec.md) |

## 開発

```sh
npm ci
npm run build
npm test
npm --prefix tools/rag ci
npm --prefix tools/rag test
npm run test:package
npm run bench -- 1000 300
```

`test:package` は `dist/packages/` に tarball を作り、空のプロジェクトへオフラインでインストールして検索と本文取得を試します。ベンチマークは合成データで索引作成、検索レイテンシ、メモリ、Recall@10、nDCG@10 などを計測します。変更前の確認と CI の起動方法は [開発と変更の確認](CONTRIBUTING.md) にあります。

## 現在の状態

- バージョン 0.5.0 の開発版です。コマンドや設定の形は変わる可能性があります。
- `dunlin` は仮のパッケージ名です。npm への公開は準備中です。本体のライセンスは MIT です。
- 動作を確認している環境は、Windows x64、Linux x64（glibc）、macOS arm64 と x64 です。
- 版の保管と、それを使うコマンド（`list`、`results`、`optimize`、`release`）は `data-management` ブランチで実装中です。コマンドは、読む設定ファイルの形で仕組みを選びます。

| | 新しい形（段階ごとの並びで書く設定） | `init` が作る形（`modules:` と `use:`） |
|---|---|---|
| 読める資料 | `.md`、`.markdown`、`.txt` | 左に加え HTML、PDF、Word、Excel、PowerPoint、OCR |
| 検索 | キーワード | キーワード、ベクトル、ハイブリッド、リランク、ユーザー辞書 |
| 版の記録と再利用、`list`、`results`、`optimize`、`release` | ○ | — |
| `serve`（MCP・HTTP）、`ask` | まだ | ○ |

Wiki と PARA は、どちらの形とも別の仕組みで、いつでも使えます。2つの形の詳しい違いは [入門の付録](docs/getting-started.md#付録-2つの設定の形) にあります。
