# PARA 入門：AI に、資料とメモを「行動に沿って」整理させる

[English](start-para.en.md) · [入門の一覧](getting-started.md)

溜まった資料やメモを AI に読ませ、PARA の4分類（Projects・Areas・Resources・Archives）にノートとして整理させます。ノートのすべての文には、元の資料への引用が付きます。人はブラウザで読み、エージェントは MCP で同じノートを引けます。

## PARA という考え方

PARA は、Tiago Forte が提唱した、デジタルの情報を整理する方法です（[解説](https://fortelabs.com/the-p-a-r-a-method-a-universal-system-for-organizing-digital-information-75a9da8bfb37/)）。

中心の考えは、**テーマではなく「行動にどれだけ近いか」で分ける**ことです。「ポンプ」「規程」「顧客A」のようにテーマで分けると、フォルダは増え続け、いま使うものが埋もれます。PARA は「この情報を、いまの仕事に使うか」で、4つに分けます。

| 分類 | 入るもの | 終わりがあるか | 例 |
|---|---|---|---|
| **Projects** | 目標と期限のある取り組み | ある | 来月のポンプ更新工事、新人研修の準備 |
| **Areas** | 続けて責任を持つ領域 | ない | 設備の定期保全、安全管理 |
| **Resources** | いつか役立つかもしれない参考情報 | ない | 部品メーカーのカタログ、業界の規格 |
| **Archives** | 上の3つで、もう動いていないもの | — | 終わった工事の記録、担当を外れた領域 |

上ほど行動に近く、下ほど遠い並びです。

- **Projects と Areas の違い**は終わりがあるかどうかです。「工事を終える」は Projects、「設備を保ち続ける」は Areas です。
- **Areas と Resources の違い**は責任があるかどうかです。自分が守る基準のある領域は Areas、関心があるだけの話題は Resources です。
- **情報は動く**。工事が終われば、その資料は Projects から Archives へ移ります。分類は固定の棚ではなく、いまの仕事を映すものです。

## Wiki との違い

| | Wiki | PARA |
|---|---|---|
| まとまりの単位 | テーマごとのページ | ノート |
| いちばん上の区分 | テーマの区分 | Projects、Areas、Resources、Archives |
| アーカイブ | 古くなったページをしまう | 終わった取り組みを Archives へ移す |
| トップページ | 各区分に何が入っているか | 4区分の説明と、進行中の Projects の一覧 |

コマンド、画面、引用の決まりは Wiki と共通です。「知識の事典」を作るなら Wiki、「仕事の机」を整えるなら PARA です。

## こんなときに

- 調べもののメモや資料が溜まり、どこに何があるか分からない
- 案件ごとの資料と、続けて担当する業務の資料を分けて持ちたい
- 終わった案件の資料を、消さずに片づけたい

## 1 執筆モデルを用意する

ここでは、手元で動く Ollama の `qwen3:4b` を使います。

```sh
nest config set llm --provider ollama --model qwen3:4b
```

## 2 PARA を作る

```sh
nest init para ./para --purpose "設備保全の仕事に関わる資料とメモを整理する"
cd para
```

`init para` は、`site/` の下に4分類のディレクトリを作ります。

`--purpose` は、AI が分類を決めるときの基準になります。いま進めている取り組みや、担当している領域を書いておくと、AI が Projects と Areas を見分けやすくなります。たとえば「来月のポンプ更新工事を進め、設備の定期保全を担当している」のように書きます。あとで `KnowledgeNestfile.yaml` の `about.purpose` で直せます。

## 3 資料を取り込む

```sh
nest ingress ../inbox
```

AI が資料を読み、ノートを書いて、4分類のどこかに置きます。`nest ingress https://...` のように URL を渡すと、Web のページも取り込めます。書いた文の引用は検証され、資料と一字一句一致しない文は保存されません。時間がかかるときは、表示された作業 ID を `nest progress <作業ID>` に渡して進み具合を見られます。

## 4 読む：ブラウザと MCP

```sh
nest serve
```

`serve` は、ノートを Web と MCP の両方で提供します。

- **ブラウザで**: `http://127.0.0.1:4319` を開くと、左のファイルツリーのいちばん上に Projects・Areas・Resources・Archives が並び、開いたノートや原本は右に出ます。左上の **資料を取り込む** に URL かファイルを渡すと、そのまま取り込めます。トップページには、4区分の説明と進行中の Projects が出ます。
- **エージェントから**: MCP クライアントの設定に登録すると、Claude などが、あなたの整理したノートを根拠に答えます。

## 5 使い続ける

| したいこと | コマンド |
|---|---|
| ノートを根拠に質問する | `nest query "来月の工事で交換する部品は？"` |
| 分類や書き方を直させる | `nest instruct <ノート> "これは Areas に移して"` |
| 今後の分け方の決まりを足す | `nest instruct --persistent "カタログ類は Resources に置く"` |
| 終わった取り組みを片づける | `nest archive <ノート>`（Archives へ移る。`--restore` で戻せる） |
| 引用切れや食い違いを見つける | `nest verify`、`nest verify --content` |
| 作業を取り消す | `nest undo <作業ID>` |

アーカイブしたノートは、通常の質問の対象から外れます。消えるわけではなく、読むことも戻すこともできます。

## 6 うまくいかないとき

| 症状 | 対処 |
|---|---|
| 思った分類に入らない | `--purpose` に進行中の取り組みと担当領域を書く。決まりを `instruct --persistent` で足す |
| `MODEL_REQUIRED` | `models.writer.model` を設定する |
| `INVALID_CITATION` | 引用がどれも資料と一致しなかった。もう一度 `ingress` するか、より大きなモデルを使う |

そのほかは [Wiki 入門](start-wiki.md#9-うまくいかないとき) と [困ったときは](troubleshooting.md) にあります。

## 次に読むもの

- 指示、点検、取り消しの詳細（Wiki と共通）：[Wiki 入門](start-wiki.md)
- 画面の操作：[Wiki・PARA の使い方](wiki.md)
