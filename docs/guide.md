# 検索を数字で良くする：変えて、測って、比べて、リリースする

[English](guide.en.md)

「検索が外れる気がする。チャンクを小さくすれば良くなる？」——その答えを、勘ではなく数字で出すための文書です。

検索の良し悪しは、見た目では分かりません。ある質問で良くなった設定が、別の質問を外していることもあります。Knowledge-Nest では、質問と答えの組を登録しておけば、設定を変えるたびに「何問中何問の答えを拾えたか」を測れます。しかも、変えた所から後ろしか作り直さないので、試すのは速く、何通りでも比べられます。

MCP・RAG の検索を、まず [MCP 入門](start-mcp.md) か [RAG 入門](start-rag.md) で動かしてから読むと分かりやすい文書です。2章で最初の設定を書き、3章から先で改善の進め方を説明します。

> この文書の新しいコマンドは、いまはテキストと Markdown（`.md`、`.markdown`、`.txt`）、キーワード検索、根拠の回収率に対応しています。ベクトル検索、ユーザー辞書、PDF や Office の読み込み、`serve` などは `init` が作る形の設定で使います（[10 従来の流れで使うもの](#10-従来の流れで使うもの)）。

## 1 考え方

```text
資料 → extract → transform → index → search → eval
      (文書)     (チャンク)   (索引)   (検索)   (結果)
```

- 段階の結果は、版として保管に残ります（`.knowledge-nest/store`）。同じ入力と同じ処理なら、2度は実行されません。
- 設定を変えると、**変えた段階から後ろだけ**が動きます。たとえば `transform` を変えても、文書の読み取りは再利用されます。
- 結果には名前を付けられます。名前は種類ごと（文書、チャンク、索引、評価データ、評価の結果）に別の棚にあります。
- 改善は、基準を測り、1か所を変えて測り、比べて選ぶ、という繰り返しです。

## 2 最初の設定を書いて、測る

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

`Recall`（回収率）は、正解の引用が検索結果に入っていた質問の割合です。質問を増やし、設定を変えて測り直し、比べて選ぶ。その進め方を、次の章から説明します。

## 3 評価データを用意する

質問と、答えが書かれている引用の組です。JSON の配列にして、`eval-data import` で保管に入れます。

```json
[
  { "id": "pump-check", "question": "ポンプの点検", "source": "docs", "document": "manuals/pump.md",
    "quote": "ポンプの点検は毎月最初の営業日に行う。", "split": "dev" },
  { "id": "pump-seal", "question": "シールの交換", "source": "docs", "document": "manuals/pump.md",
    "quote": "型番 P-200 のシールは、2年ごとに交換する。", "split": "dev" }
]
```

```sh
nest eval-data import cases.json
nest eval-data import cases.json --tag v2
```

- `source` と `document` は、文書の場所です。`source` は `source.filesystem` の `id`（省略時はフォルダ名）、`document` はそのフォルダの中の位置です。
- `quote` は、その文書に1か所だけ現れる引用にします。
- 質問は、利用者が書く言い回しに近づけます。資料の文をそのまま質問にすると、外れにくい評価になり、改善の手がかりが減ります。
- 調整は `split: dev` のケースで行い、`split: test` のケースは、採用の前に1回だけ測ります。
- 同じ中身を取り込み直しても、新しい版はできません。ケースを足すと新しい版ができ、名前（`default` または `--tag`）がそちらを指します。

> 質問と引用の候補をモデルに作らせる `eval-data generate` は、従来の流れのコマンドです（[evaluation.md](evaluation.md)）。作った候補を JSON に書き出し、新しい流れに取り込めます。

## 4 基準を測り、外れを読む

設定に評価の区切りを足します。

```yaml
eval:
  - eval.evidence: { data: default, split: dev }
```

```sh
nest eval
nest results
```

`eval` は、必要な段階を作ってから測り、回収率を表示します。

```text
Recall: 0.667 (2 of 3 cases)
```

`nest results` は、名前の付いた評価の結果を、回収率の高い順に並べます。結果の名前を渡すと、ケースごとの順位が見えます。

```sh
nest results trial-400
```

```text
Result version 7 (trial-400): recall 0.667 (2 of 3), index main-400 with data default
Found at  Case         Question
missed    filter       フィルターの交換
1         pump-check   ポンプの点検
2         pump-seal    シールの交換
```

- `Found at` は、正解の引用が検索結果の何番目に入っていたかです。`missed` は入っていなかった質問で、先頭に並びます。
- 外れた質問を `nest search "<質問>"` で再現し、原因を探します。

| 外れ方 | 試すこと |
|---|---|
| 正解の箇所が長いチャンクに埋もれる | `chunk.length` の `size` と `overlap` を変える。見出しのある文書は `chunk.structure` |
| 「2年ごとに交換する」だけでは何の話か分からない | チャンクの文脈（5章） |
| 正解は取れているが、順位が低い | `retrieval.keyword` の `top_k` を変える |

## 5 1か所を変えて比べる（手で）

基準の `KnowledgeNestfile.yaml` をコピーし、変えたい段階だけを書き換えます。基準の名前を動かさないよう、変えた段階にはタグを付け直し、文書は基準の名前から始めます（`from`）。

```yaml
# candidate.yaml
extract:
  - from: docs

transform:
  - chunk.length: { unit: characters, size: 400, overlap: 50 }
  - tag: chunks-400

index:
  - index.keyword: { tokenizer: lindera }
  - tag: main-400

search:
  - retrieval.keyword: { top_k: 10 }

eval:
  - eval.evidence: { data: default, split: dev }
  - tag: trial-400
```

```sh
nest eval candidate.yaml
nest results
```

- `extract` の `from: docs` は、フォルダを走査せず、`docs` という名前の文書の版を使います。基準と同じ文書で測れます。
- 結果は `trial-400` の名前で残ります。`nest results` に基準の結果と並ぶので、回収率を比べます。
- 評価の方法とデータが同じでなければ、比べても意味がありません。`eval.evidence` の `data` と `split` をそろえます。

## 6 チャンクに文脈を持たせる

チャンクは、文書から切り出すと、どの文書のどの節にあったかを失います。`context` を指定すると、その情報を検索用の本文の先頭に付けます。

```yaml
transform:
  - chunk.structure:
      max_chars: 800
      context: { document: true, headings: true, prepend: true }
```

| 引数 | 意味 |
|---|---|
| `headings` | チャンクより上にある見出しを、チャンクの情報に記録する |
| `document` | 文書の名前も文脈に含める（`prepend` と一緒に使う） |
| `prepend` | 索引が検索する本文の先頭に、文脈の1行を付ける |

- 検索結果の本文と位置は変わりません。変わるのは、索引が検索する文字列だけです。
- 見出しを読むのは Markdown の `#` だけです。ほかの形式では空になります。
- `context` を変えると、チャンクと索引を作り直します。文書は読み直しません。

## 7 たくさんの設定を、まとめて試す（optimize）

候補を手で1つずつ作る代わりに、変えるところを指示に書くと、Knowledge-Nest が候補を作って順に測ります。

```yaml
# tune.yaml
optimize:
  base: KnowledgeNestfile.yaml
  vary:
    - name: chunking
      options:
        as-is: {}
        length:
          transform:
            - chunk.length: { unit: characters, size: [200, 400], overlap: 0 }
        structure:
          transform:
            - chunk.structure: { max_chars: 800 }
    - name: search
      options:
        top10: {}
        top3:
          search:
            - retrieval.keyword: { top_k: 3 }
  eval:
    data: default
    split: dev
  limit:
    candidates: 20
```

```sh
nest optimize tune.yaml
```

```text
[1/N] 000 base
      recall 0.667
...
```

進み具合は1行ずつ表示されます（表示の例。数や名前は指示によります）。終わると、回収率の高い順の表になります。

```text
Trial  Recall  To base  Ran                  Time   What
010    1.000   +0.333   transform+index+eval ...    chunking=length(size=200),search=top10
000    0.667   -        nothing              ...    base
```

- **軸**は変えるところ、**選択肢**は軸ごとの候補です。選択肢を1つずつ選んだ組み合わせが、1つの試行です。上の例は、チャンクの4通り（`as-is`、`length` の2つのサイズ、`structure`）と検索の2通りを組み合わせ、基準（0番）と合わせて試します。
- 引数の値を配列にすると（`size: [200, 400]`）、その場で選択肢が増えます。
- 空の選択肢（`{}`）は、基準のままです。基準（0番）は必ず測られ、比べる相手になります。
- 選択肢は、区切りをまるごと書き換えます。同じ区切りを書き換える軸が2つあると、設定の誤りで止まります。`eval`、`tag`、`from` は書き換えられません。
- 試行の途中までが同じなら、そこまでを共有します。同じチャンクを使う試行は、チャンクを1回だけ作ります。
- `limit.candidates` は、試す候補の数の上限です。
- 試行が失敗したら理由を記録して次へ進みます。続けて3回失敗すると、残りは実行しません。
- 同じ指示をもう一度実行すると、済んだ試行は再利用され、残りだけが動きます。
- 最適化は、各種類の `default` を動かしません。

結果を見直すときは、次のコマンドです。

```sh
nest results            # 評価の結果と、最適化の名前の一覧
nest results tune       # 最適化 tune の試行の表
nest results tune/010   # 1つの試行のケースごとの結果
```

`--name` を付けないときの最適化の名前は、指示のファイル名（拡張子なし）です。

試行ごとの設定ファイルは `.knowledge-nest/trials/<名前>/<番号>.yaml` に書かれます。気になる試行だけをやり直すには、そのファイルを指定します。

```sh
nest eval .knowledge-nest/trials/tune/010.yaml
```

> 軸にできるのは、いまのところ `transform`、`index`、`search` です。`extract` を書き換える選択肢、回収率以外の指標、前の結果から次の値を選ぶ探索は、まだありません。

## 8 リリースする

提供する索引が切り替わるのは、あなたが明示したときだけです。`build`、`eval`、`optimize` は、どれも提供を動かしません。資料を足して作り直しても、確かめて `release` するまでは、前の索引が提供され続けます。文書の削除や設定の誤りが、確かめる前に利用者へ届くことはありません。

```sh
nest build                  # 作る。提供は動かない
nest list indexes           # できた索引と、いま提供している索引を見る
nest search "..." --index main
nest eval                   # 確かめる
nest release main           # 確かめた索引を提供する
```

| 操作 | 提供 |
|---|---|
| `nest build`、`nest eval`、`nest optimize` | 動かさない |
| `nest build --release` | 作り終えた索引へ切り替える。作るたびに提供したいときの近道 |
| `nest release <索引> [--name <名前>]` | 指した索引へ切り替える。名前の既定は `release` |

- **名前でも番号でも指せる。** `release` には、索引の名前（`main`、`tune/010`）のほか、`nest list indexes` に出る版の番号も渡せます。`v7` と表示された索引なら `nest release 7` です。
- **前の版に戻す。** 戻したい索引をもう一度 `release` します。どの版からどの版へ切り替えたかが表示され、記録は消えません。
- `nest list indexes` に、どの索引がどの名前で提供されているかが出ます。

> `serve` は、この提供用の名前をまだ読みません。MCP や HTTP で提供するときは、従来の流れを使います。

## 9 資料の変更の取り込み方を選ぶ

`source.filesystem` の `apply` で、フォルダの変更のうち取り込むものを選べます。

```yaml
extract:
  - source.filesystem: { path: ./docs, apply: [add, update] }
  - loader.text: {}
  - tag: docs
```

- `add`（追加）、`update`（更新）、`delete`（削除）から選びます。省略するとすべてを取り込みます。
- 取り込まなかった変更は、`build` の表示に「Not taken in」として出ます。次に取り込む設定にすると反映されます。
- 例えば `[add, update]` にすると、フォルダから消えた文書も検索に残ります。

## 10 従来の流れで使うもの
次のものは、新しい流れではまだ使えません。`nest init` が作る従来の設定で使います。

| やりたいこと | 従来のコマンド・文書 |
|---|---|
| ベクトル検索、ハイブリッド検索、リランク | 埋め込みの設定と `index.vector`、`fusion.rrf`。[MCP と RAG](mcp.md) · [モジュール](modules.md) |
| ユーザー辞書 | `dictionary suggest` と `dictionary add`。[検索評価とユーザー辞書](evaluation.md) |
| 質問の自動生成と確認 | `eval-data generate` と `eval-data review`。[検索評価とユーザー辞書](evaluation.md) |
| PDF、Office、HTML、OCR | `loader.auto` と `nest add ocr`。[PDF の抽出](pdf-extraction.md) |
| MCP・HTTP での提供、`ask` | `serve`、`ask`。[MCP と RAG](mcp.md) · [提供する入口と、接続を許す相手を決める](serve.md) |
| エージェントとの調整 | [エージェントと試行する](agents.md) |

どちらの流れかは、コマンドが読む設定ファイルの形で決まります。段階が `- source.filesystem: ...` のような並びなら新しい流れ、`modules:` と `use:` なら従来の流れです。1つのプロジェクトに、両方の形のファイルを置いても構いません。

2つの形の違いは次のとおりです。

検索（`mcp`・`rag`）のコマンドは、読む設定ファイルの形で、どの仕組みを使うかを選びます。

| 設定ファイルの形 | 例 | 仕組み | 使えるコマンド |
|---|---|---|---|
| `modules:` と `use:` を使う形 | `nest init mcp`・`init rag` が作るもの（MCP 入門、RAG 入門） | これまでのビルド | `build`、`search`、`eval`、`eval-data`、`source`、`transform`、`index`、`status`、`use`、`tag`、`serve`、`ask` |
| 段階ごとの並び（`extract:` の下に `- source.filesystem: ...`） | 2章の設定 | 版の保管（`.knowledge-nest/store`） | `build`、`search`、`eval`、`eval-data import`、`list`、`results`、`optimize`、`release` |

| できること | `init` が作る形 | 段階ごとの並び |
|---|---|---|
| 読める資料 | Markdown、テキスト、HTML、PDF、Word、Excel、PowerPoint、OCR | `.md`、`.markdown`、`.txt` |
| 検索 | キーワード、ベクトル、ハイブリッド、リランク、ユーザー辞書 | キーワード |
| 版の記録と再利用、候補の一括比較 | — | ○ |
| `serve`（MCP・HTTP）、`ask` | ○ | まだ |

- 段階ごとの並びでは、`build` の `--from`、`--until`、`--force`、`--input`、`--dry-run`、`--skip-unsupported`、`--tag`、`eval` の `--build`、`--split`、`--tag` はまだ使えません。
- 1つのフォルダに、両方の形の設定ファイルを置いても構いません。
- Wiki と PARA は、どちらの形とも別の仕組みです。

## 11 チェックリスト

- [ ] 評価データが、実際の質問に近い言い回しで10件以上ある
- [ ] 基準を `nest eval` で測り、結果に名前がある
- [ ] 試行は、評価の方法とデータを基準とそろえた
- [ ] 平均だけでなく、外れた質問（`missed`）がどう変わったかを見た
- [ ] 採用の前に、`split: test` で測った
- [ ] `release` した索引を、`nest list indexes` で確かめた
