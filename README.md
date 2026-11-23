# Knowledge-Nest

[English](README.en.md)

**手元の資料を、AI が出典を示して使える知識にする。コマンドをいくつか打つだけで、プログラムは書きません。**

資料のフォルダ（Markdown、PDF、Word、Excel、Web のページ …）を渡すと、Knowledge-Nest が AI の使える形に整えます。資料も索引も手元のフォルダに置いたまま、答えにもページにも、いつも出典が付きます。

## 1つの道具で、4通りの文書データベース

ふつうは、ベクトルデータベース、RAG のフレームワーク、Wiki の道具、ノートアプリをそれぞれ選んで組み合わせます。Knowledge-Nest は、同じ資料フォルダから、その4通りを1つで作れます。

```text
                                  ┌─ MCP    エージェントが引く検索
資料フォルダ ──→ Knowledge-Nest ──┼─ RAG    アプリに返す、出典付きの答え
(Markdown、PDF、Word、Excel …)    ├─ Wiki   みんなで使う知識のページ
                                  └─ PARA   自分の仕事を整理するノート
```

| やりたいこと | 作るもの | AI モデル | 使い方 |
|---|---|---|---|
| Claude などのエージェントに、手元の資料を調べさせたい | **MCP**：エージェントが引く検索 | 要らない | [MCP 入門](docs/start-mcp.md) |
| チャットアプリや社内ツールで、資料に基づく答えを返したい | **RAG**：出典付きで答える API | 回答に要る | [RAG 入門](docs/start-rag.md) |
| 資料を、チームで使う知識のページにまとめたい | **Wiki**：みんなで使う知識のページ | 執筆に要る | [Wiki 入門](docs/start-wiki.md) |
| 自分の仕事の資料とメモを、取り組みごとに整理したい | **PARA**：自分の仕事を整理するノート | 執筆に要る | [PARA 入門](docs/start-para.md) |

- **覚えることは1通り。** インストールは1つ、始め方は `nest init <形>` の1つです。資料の読み込み、出典の付け方、設定ファイル（`KnowledgeNestfile.yaml`）、`nest serve` での提供は、どの形でも共通です。
- **同じ資料から、目的に合わせて増やせる。** まず検索を作り、あとから答えの API や Wiki を、同じ資料フォルダから足せます。作り直しも乗り換えも要りません。
- **どの形も、エージェントから引ける。** 4つとも MCP で提供するので、Claude などは、資料そのものも、AI がまとめた知識も、同じつなぎ方で使えます。
- **手元で完結する。** 資料も索引もプロジェクトのフォルダに保存されます。外へ送るのは、あなたが AI モデルを設定したときに、その処理に要るテキストだけです。

迷ったら、モデルの準備が要らない MCP から始めてください。インストールは [こちら](#インストール)、順を追った説明は [入門](docs/getting-started.md)、4つの形の詳しい違いは [4つの形の違い](docs/forms.md) にあります。

## しかも、1つずつ見ても選ぶ理由がある

まとめて作れるだけではありません。それぞれの形を、同じ種類のほかのライブラリや道具と比べても、Knowledge-Nest を選ぶ理由があります。ほかを選ぶほうがよい場合も含めた比較は [Knowledge-Nest が向いている構成](docs/comparison.md) にあります。

### MCP：エージェントが引く検索

比べる相手：ファイルシステムの MCP サーバー（エージェントがファイルを直接読む）、ベクトルデータベースの MCP サーバー（Chroma、Qdrant など）

| ほかの MCP サーバーでは | Knowledge-Nest では |
|---|---|
| ファイルを直接読ませると、PDF や Office は扱いにくく、大きな資料はエージェントの文脈を埋める | PDF・Office・スキャンした資料も本文にし、答えのある箇所だけを、資料名と行を付けて返す |
| ベクトル検索には埋め込みモデルの準備が要り、型番や専門用語の完全一致は苦手なことがある | 埋め込みモデルなしで、日本語のキーワード検索がすぐ動く。ベクトルを足してハイブリッドにもできる |
| 資料の読み込み、分割、更新の反映は、自分で組む | 資料フォルダを渡せば、読み込みから分割、変わった文書だけの更新まで行う |
| 検索が当たっているかは、使って感じるしかない | 回収率で測り、候補の設定をまとめて試して、最適化まで行う |

[MCP 入門](docs/start-mcp.md)

### RAG：出典付きで答える API

比べる相手：LangChain や LlamaIndex などの RAG フレームワーク、画面で資料を登録するノーコードの RAG 製品

| ほかの RAG では | Knowledge-Nest では |
|---|---|
| 読み込み、分割、索引、検索、生成をコードで組み、API も自分で立てる | 設定ファイルとコマンドで、OpenAI の SDK でそのまま呼べる API まで立つ |
| 根拠が見つからない質問への振る舞いや、出典の付け方は自分で作る | 根拠がなければ答えずに断り、出典は資料の版と原文の範囲まで指す |
| 答えの評価は、別の評価ライブラリを組み合わせる | 評価用の質問づくりから、測定、候補の一括比較、最適化までが同じ道具に入っている |
| ノーコードの製品は、資料をその製品のサーバーに登録して使う | 資料も索引も手元のフォルダに置いたまま動く |

[RAG 入門](docs/start-rag.md)

### Wiki：みんなで使う知識のページ

比べる相手：Karpathy の LLM-Wiki を実装したスクリプト、ノートアプリの AI 機能（Notion、Obsidian など）、資料に質問するチャット

| ほかの道具では | Knowledge-Nest では |
|---|---|
| 資料に質問するチャットは、答えがその場で消える | 読んだ結果が出典付きのページとして積み上がり、資料が改訂されれば、そのページだけを書き直す |
| ノートアプリの AI が書いた文は、どこまでが資料にあることか分からない | 数字・日付・引用が出典の資料にあるかを、プログラムが取り込みのたびに照らす。ページ同士の矛盾やページのない用語も挙げる |
| LLM-Wiki のスクリプトは、取り込みと書き出しが中心 | 点検、作業ごとの取り消し、目録と作業の記録、分類、ブラウザの画面、MCP での提供、ログインの要るページや X の取り込みまでそろう |
| 資料にない現場の知見は、本文に混ざって区別がつかない | 誰の補足かを付けて、出典の1つとして残す |

Karpathy の LLM-Wiki の考え方に、出典との照合を足したものです。 [Wiki 入門](docs/start-wiki.md)

### PARA：自分の仕事を整理するノート

比べる相手：Notion や Obsidian で、テンプレートや手作業で PARA を運用する方法

| ノートアプリの PARA では | Knowledge-Nest では |
|---|---|
| 資料が来るたびに、どの取り組みに使うかを人が考えて振り分ける | AI が資料を読み、使う取り組みにノートを書いて置く |
| 振り分けはフォルダに入れて終わり | ノートに「この資料を、この取り組みにどう使うか」まで書く |
| AI に振り分けを任せると、フォルダが勝手に増えやすい | 置けるのは、人が `rules.md` に書いた取り組みと領域だけ |
| 取り組みが終わったら、ノートや資料を手で片付ける | `nest archive` で、ノート、資料の写し、リンクを一度に Archives へ移し、`nest undo` で戻せる。期限を過ぎた取り組みも知らせる |

エージェントも MCP で、いまの取り組みのノートを根拠に答えます。 [PARA 入門](docs/start-para.md)

## 組み替えて、足せる（MCP・RAG）

MCP と RAG の処理は、読み込み、チャンク分け、索引、検索、回答の段ごとに、モジュールを並べた設定でできています。

- **組み替える。** ベクトル検索やリランカーを足す、チャンクを見出しに沿わせる、といった変更は設定を数行書き換えるだけです。 [モジュール](docs/modules.md)
- **読めない形式は、手持ちの変換ツールで。** `loader.command` に変換コマンドを書けば、その出力を本文として取り込めます。
- **モデルは差し替えられる。** 埋め込みと回答は手元の Ollama か OpenAI などの API から、リランカーは `/v1/rerank` か `/rerank` を出すサーバーから選べます。
- **新しいモジュールを足したいときは、どうぞ。** `src/registry.rs` の登録表に定義して足します。本体は MIT ライセンスで、手順は [開発と変更の確認](CONTRIBUTING.md) にあります。プルリクエストを歓迎します。

検索を測って最適化する手順は [検索を数字で良くする](docs/guide.md) にあります。

## インストール

npm への公開は準備中です。現在はソースからビルドします。Rust 1.95 以降と、Node.js 22.20 以降の 22 系または 24.12 以降の 24 系が必要です。

```sh
git clone https://github.com/hachiware-labs/knowledge-nest
cd knowledge-nest
npm ci
npm run build
npm link
nest --help
```

## ドキュメント

| やりたいこと | 文書 |
| --- | --- |
| 初めて使う | [入門](docs/getting-started.md)（やりたいことから各入門へ）。4つを一度に試すなら [サンプル資料で始める](docs/quickstart.md) |
| Claude に調べさせる・アプリに答えさせる | [MCP 入門](docs/start-mcp.md) · [RAG 入門](docs/start-rag.md) · [MCP と RAG の起動](docs/mcp.md) |
| ナレッジベースを作る・資料とメモを整理する | [Wiki 入門](docs/start-wiki.md) · [PARA 入門](docs/start-para.md) · [Wiki・PARA の使い方](docs/wiki.md) |
| 検索を数字で良くする | [検索を数字で良くする](docs/guide.md) · [検索評価とユーザー辞書](docs/evaluation.md) · [エージェントと試行する](docs/agents.md) · [精度を上げる手法の候補](docs/retrieval-techniques.md) |
| ほかのマシンから使わせる | [提供する入口と、接続を許す相手を決める](docs/serve.md) |
| 4つの形とほかの道具を比べる | [4つの形の違い](docs/forms.md) · [Knowledge-Nest が向いている構成](docs/comparison.md) · [ベンチマーク](docs/benchmarks.md) |
| コマンドや設定を調べる | [CLI](docs/cli.md) · [モジュール](docs/modules.md) · [KnowledgeNestfile の既定値](docs/config.md) · [困ったときは](docs/troubleshooting.md) |
| Node.js に組み込む | [Node.js API](docs/api.md) |
| 仕組みを知る | [ビルドと評価の関係](docs/concepts.md) · [データ管理システム](docs/data-management.md) · [実装構成](docs/architecture.md) · [PDF の抽出](docs/pdf-extraction.md) · [設計仕様](docs/knowledge-nest-spec.md) |
| 開発・運用を確認する | [開発と変更の確認](CONTRIBUTING.md) · [変更履歴](CHANGELOG.md) · [セキュリティ](SECURITY.md) · [受け入れ確認](docs/acceptance.md) |
| ライセンスと通知を調べる | [本体のライセンス状況](LICENSE) · [第三者の通知](THIRD_PARTY_NOTICES.md) |

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
- `knowledge-nest` は仮のパッケージ名です。npm への公開は準備中です。本体のライセンスは MIT です。
- 動作を確認している環境は、Windows x64、Linux x64（glibc）、macOS arm64 と x64 です。
- 版の保管と、それを使うコマンド（`list`、`results`、`optimize`、`release`）は `data-management` ブランチで実装中です。コマンドは、読む設定ファイルの形で仕組みを選びます。

| | 新しい形（段階ごとの並びで書く設定） | `init` が作る形（`modules:` と `use:`） |
|---|---|---|
| 読める資料 | `.md`、`.markdown`、`.txt` | 左に加え HTML、PDF、Word、Excel、PowerPoint、OCR |
| 検索 | キーワード | キーワード、ベクトル、ハイブリッド、リランク、ユーザー辞書 |
| 版の記録と再利用、`list`、`results`、`optimize`、`release` | ○ | — |
| `serve`（MCP・HTTP）、`ask` | まだ | ○ |

Wiki と PARA は、どちらの形とも別の仕組みで、いつでも使えます。2つの形の詳しい違いは [検索を数字で良くする](docs/guide.md#10-従来の流れで使うもの) の10章 にあります。
