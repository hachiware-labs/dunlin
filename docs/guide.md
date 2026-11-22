# 使いこなす：変えて、測って、比べて、リリースする

[English](guide.en.md)

「検索が外れる気がする。チャンクを小さくすれば良くなる？」——その答えを、勘ではなく数字で出すための文書です。

検索の良し悪しは、見た目では分かりません。ある質問で良くなった設定が、別の質問を外していることもあります。Knowledge-Nest では、質問と答えの組を登録しておけば、設定を変えるたびに「何問中何問の答えを拾えたか」を測れます。しかも、変えた所から後ろしか作り直さないので、試すのは速く、何通りでも比べられます。

[入門](getting-started.md#3-検索を数字で良くする新しいコマンド) の3章で `build` と `eval` が動いた人を想定しています。

> この文書の新しいコマンドは、いまはテキストと Markdown（`.md`、`.markdown`、`.txt`）、キーワード検索、根拠の回収率に対応しています。ベクトル検索、ユーザー辞書、PDF や Office の読み込み、`serve` などは `init` が作る形の設定で使います（[9 従来の流れで使うもの](#9-従来の流れで使うもの)）。

## 1 考え方

```text
資料 → extract → transform → index → search → eval
      (文書)     (チャンク)   (索引)   (検索)   (結果)
```

- 段階の結果は、版として保管に残ります（`.knowledge-nest/store`）。同じ入力と同じ処理なら、2度は実行されません。
- 設定を変えると、**変えた段階から後ろだけ**が動きます。たとえば `transform` を変えても、文書の読み取りは再利用されます。
- 結果には名前を付けられます。名前は種類ごと（文書、チャンク、索引、評価データ、評価の結果）に別の棚にあります。
- 改善は、基準を測り、1か所を変えて測り、比べて選ぶ、という繰り返しです。

## 2 評価データを用意する

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

## 3 基準を測り、外れを読む

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

## 4 1か所を変えて比べる（手で）

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

## 5 チャンクに文脈を持たせる

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

## 6 たくさんの設定を、まとめて試す（optimize）

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

## 7 リリースする

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

## 8 資料の変更の取り込み方を選ぶ

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

## 9 従来の流れで使うもの

次のものは、新しい流れではまだ使えません。`nest init` が作る従来の設定で使います。

| やりたいこと | 従来のコマンド・文書 |
|---|---|
| ベクトル検索、ハイブリッド検索、リランク | 埋め込みの設定と `index.vector`、`fusion.rrf`。[MCP と RAG](mcp.md) · [モジュール](modules.md) |
| ユーザー辞書 | `dictionary suggest` と `dictionary add`。[検索評価とユーザー辞書](evaluation.md) |
| 質問の自動生成と確認 | `eval-data generate` と `eval-data review`。[検索評価とユーザー辞書](evaluation.md) |
| PDF、Office、HTML、OCR | `loader.auto` と `nest add ocr`。[PDF の抽出](pdf-extraction.md) |
| MCP・HTTP での提供、`ask` | `serve`、`ask`。[MCP と RAG](mcp.md) · [11 提供する入口と、接続を許す相手を決める](#11-提供する入口と接続を許す相手を決める) |
| エージェントとの調整 | [エージェントと試行する](agents.md) |

どちらの流れかは、コマンドが読む設定ファイルの形で決まります。段階が `- source.filesystem: ...` のような並びなら新しい流れ、`modules:` と `use:` なら従来の流れです。1つのプロジェクトに、両方の形のファイルを置いても構いません。

## 10 Wiki と PARA を使いこなす

Wiki と PARA は検索の知識とは別の仕組みで、この文書の版の保管には載っていません。使い方は [Wiki・PARA の使い方](wiki.md) にあります。要点です。

- 取り込みは `nest ingress <資料>` です。引用が資料と一字一句一致しない記述は保存されず、`CITATION_REJECTED` の警告で示されます。
- `nest verify` で引用・原本の版・リンクを、`nest verify --content` でモデルを使ってページ間の食い違いを調べます。
- ページへの指示は `nest instruct <ページ> "指示"`、今後も使う決まりは `nest instruct --persistent "決まり"` です。
- 変更は作業単位で記録されます。望ましくなければ `nest undo <作業ID>` で戻せます。原本は書き換えられません。
- 長い取り込みは `nest progress <作業ID>` で進み具合を確認できます。
- `serve` はページを Web と MCP の両方で提供します。人がブラウザで直した内容は、そのままエージェントが MCP で引く知識になります。

## 11 提供する入口と、接続を許す相手を決める

`nest serve` で何を出し、誰からの接続を受けるかは、`KnowledgeNestfile.yaml` の `serve` に書きます。`init` が既定値をすべて書き出すので、変えたい所だけを直します。次は RAG の既定値です。

```yaml
serve:
  transport: http
  host: 127.0.0.1          # 待ち受けるアドレス。既定では自分のマシンからだけ
  port: 4318
  endpoints:               # 出す入口。書かなかった入口は立ち上がらない
    - responses            # POST /v1/responses：出典付きの答え
    - search               # POST /v1/search：検索結果と文脈
    - mcp                  # /mcp：エージェント向けの MCP（HTTP）
  access:
    allowed_hosts:         # 要求の宛先名（Host）として受け付けるもの
      - localhost
      - 127.0.0.1
    allowed_origins: []    # 別のページのブラウザから呼んでよい送信元
    auth:
      type: none           # none か token
      token_env: null      # token のとき、合言葉を入れた環境変数の名前
  limits:
    max_request_bytes: 1048576
    request_timeout_seconds: 120
    max_concurrent: 8
```

**入口（`endpoints`）** は、種別ごとに選べるものが決まっています。

| 種別 | `transport` | 選べる `endpoints` | 既定のポート |
|---|---|---|---|
| `mcp` | `stdio` だけ | なし（`host` から `limits` までを書くと `ARG_CONFLICT`） | なし |
| `rag` | `http` | `responses`、`search`、`mcp` | 4318 |
| `wiki`、`para` | `http` | `web` | 4319 |

- 回答 API を出さず、検索だけを出すときは `endpoints: [search, mcp]` にします。
- 種別にない入口を書くと `ARG_RANGE` になります。

**接続を許す相手（`access`）** は、次の順に確かめられます。

- **接続元。** `allowed_clients` を書くと、そこに挙げたアドレスの範囲と、サーバーを動かしているマシン自身からの接続だけを受け付けます。ほかは `403 CLIENT_FORBIDDEN` です。`192.168.11.0/24`（`192.168.11.*` のこと）のように、アドレスと、先頭から一致させるビット数で書きます。1台だけなら `192.168.11.7` のようにアドレスだけを書きます。書かなければ、接続元では絞りません。
- **宛先名。** 要求の `Host` が `allowed_hosts` にないと `403 HOST_FORBIDDEN` です。ホスト名か IP アドレスを、ポートを付けずに書きます。社内の名前（`nest.example.internal` など）で呼ばせるときは、その名前を足します。
- **送信元。** ブラウザが付ける `Origin` が自分自身でも `allowed_origins` にもないと `403 ORIGIN_FORBIDDEN` です。`https://app.example.com` のように、スキームを含め、パスを付けずに書きます。空のままなら、ほかのサイトのページからは呼べません。開いたページが手元のサーバーに勝手に要求を送るのを防ぐためです。
- **合言葉。** `host` を `127.0.0.1` などのループバック以外にするときは、`auth.type: token` が必須です。合言葉は設定ファイルに書かず、`token_env` に環境変数の名前だけを書きます。クライアントは `Authorization: Bearer <token>` を送ります。Wiki・PARA の画面は Basic 認証も受け付け、ユーザー名は `knowledge-nest`、パスワードは同じ合言葉です。

ほかのマシンに開くときの例です。

```yaml
serve:
  host: 0.0.0.0
  access:
    allowed_hosts: [nest.example.internal]
    allowed_origins: [https://app.example.com]
    auth: { type: token, token_env: NEST_TOKEN }
```

家や職場の LAN の中だけに開くときの例です。`allowed_hosts` には、このマシンの LAN でのアドレスを足します。ほかの端末は `http://192.168.11.16:4319/` のように、そのアドレスで開きます。

```yaml
serve:
  host: 0.0.0.0
  access:
    allowed_hosts: [localhost, 127.0.0.1, 192.168.11.16]
    allowed_clients: [192.168.11.0/24]
    auth: { type: token, token_env: NEST_TOKEN }
```

```sh
export NEST_TOKEN=...      # 長い乱数にする
nest validate --json       # 矛盾があれば、起動する前にここで分かる
nest serve
```

- `nest serve --host` と `--port` は、その起動に限って値を変えます。上の検査は上書き後の値にもかかります。
- `serve` の値はビルド ID や検索設定 ID に入りません。ポートや許可を変えても、作り直しや評価のし直しは要りません。
- 通信の暗号化と、接続元のアドレスの範囲による制限はありません。社内に公開するときは、前に置く中継サーバー（リバースプロキシ）に任せます。

`serve` が決めるのは、受ける側の接続だけです。モデルへの接続先は `models` の `base_url`（既定はローカルの Ollama）で決まります。取り込む Web のアドレスを許可したり制限したりする設定はありません。

[HTTP の接続制限](mcp.md#http-の接続制限) · [serve の設定項目の仕様](knowledge-nest-spec.md#275-serve-の設定項目) · [設定ファイルの全文](config.md)

## 12 チェックリスト

- [ ] 評価データが、実際の質問に近い言い回しで10件以上ある
- [ ] 基準を `nest eval` で測り、結果に名前がある
- [ ] 試行は、評価の方法とデータを基準とそろえた
- [ ] 平均だけでなく、外れた質問（`missed`）がどう変わったかを見た
- [ ] 採用の前に、`split: test` で測った
- [ ] `release` した索引を、`nest list indexes` で確かめた
