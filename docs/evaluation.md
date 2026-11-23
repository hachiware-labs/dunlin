# 検索を評価し、ユーザー辞書を試す

評価データに質問と正解の引用を1件登録します。基準の索引でその根拠を拾えなければ、ユーザー辞書を足した試行を作り、同じ質問で評価し直します。最後に、語全体と部分語で同じ文書の順位を確認します。

## 資料を用意する

CLI をインストールし、リポジトリのルートで新しい MCP プロジェクトを作ります。この例は同梱辞書を使うキーワード検索で進めます。 [CLI のインストール](../README.md#インストール)

```sh
nest init mcp ./evaluation-demo
cp examples/evaluation/product.txt evaluation-demo/docs/
cp examples/evaluation/lecture.txt evaluation-demo/docs/
cd evaluation-demo
```

コピーした資料は次の2ファイルです。

| 保存先 | 内容 |
| --- | --- |
| `docs/product.txt` | `製品東京大学は限定品` |
| `docs/lecture.txt` | `東京大学で講演します。` |

## 質問と引用を登録し、基準を測る

`product.txt` を見つけたいときの問い合わせとして「東京大学」を登録します。`--doc` は対象の文書 ID、`--quote` はその文書に一意に現れる引用です。最初の `eval` に付けた `baseline` は評価レポートのタグです。

```sh
nest build
nest eval-data add --question "東京大学" --doc "docs:product.txt" --quote "製品東京大学は限定品"
nest eval --tag baseline
nest report show baseline
```

この例では `scores` の `evidence_recall@context` が `0` になります。登録した引用が、実際に返す検索文脈に含まれなかったことを示しています。

現在提供中の基準ビルドにもタグを付けます。

```sh
nest tag baseline-build
```

## 質問と引用の候補をモデルに作らせる

質問と引用を手で書く代わりに、設定済みの対話用モデルに候補を作らせることができます。`KnowledgeNestfile.yaml` の `models` に `kind: chat` のモデルが1つだけあれば `--model` は省けます。

```sh
nest eval-data generate --count 30
```

- 評価ケースがまだない文書から先に読みます。長い文書は、ケースの引用がまだ少ない部分から、文書の中に散らばる順に読みます。`--doc` で対象を1文書に絞れます。
- 候補には質問の種類が付きます。`lexical` は引用の語を使う質問、`paraphrase` は言い換えた質問、`identifier` は型番・数値・名前で尋ねる質問です。結果の `kinds` に種類ごとの件数が出ます。`--kind paraphrase` のように指定すると、その種類だけを作ります。
- 1つの引用で答えられない質問は、3つまでの引用を持ちます。引用は別々の文書にあってもかまいません。
- 引用が原文にない、文書内で一意に決まらない、300文字を超える、質問が重複している候補は採用しません。結果の `dropped` に理由ごとの件数が出ます。
- 作られたケースの状態は `generated` で、確認するまで評価の対象になりません。モデルを使うため、読ませた範囲のテキストはそのモデルの提供元へ送られます。

候補を一覧し、妥当なものを採用します。`--case` は繰り返して指定できます。

```sh
nest eval-data review <dataset>
nest eval-data review <dataset> --case <id> --case <id> --state reviewed --reviewer <name> --reviewer-type human
```

`<dataset>` は `generate` が表示した評価データの ID です。採用しない候補は `--state rejected` にします。まとめて編集したい場合は `eval-data export` で書き出し、`eval-data import` で戻せます。

## 順位と、根拠を失った段階を読む

回収（`evidence_recall`）は、根拠が入ったかどうかだけを見ます。上位10件の中の並べ替えや、根拠をどこで失ったかは、次の2つで分かります。

**順位の指標。** `eval` の `modules` に `metric.evidence_rank` を足すと、`scores` に `mrr@k` と `ndcg@k` が加わります。`at` に見る件数を並べます。省くと `[10]` です。

```yaml
eval:
  split: dev
  modules:
    - use: metric.evidence_recall
      with: { at: [context, 1, 3, 10], short_evidence_chars: 40, min_coverage: 0.8, unresolved: count_as_missed }
    - use: metric.latency
      with: { percentiles: [50, 95, 99] }
    - use: metric.evidence_rank
      with: { at: [10] }
```

- 根拠の順位は、文脈の組み立てに渡した候補の中で、その根拠を覆うのに要る先頭からの件数です。融合と並べ替えの後の順です。
- `mrr@k` は、根拠ごとの順位の逆数（`k` 件より後や見つからないときは0）の平均です。`ndcg@k` は、すべての根拠が先頭にある並びを1とした値です。根拠を複数持つケースでは、根拠を同じ重さで数えます。
- `metric.evidence_rank` は判定を変えないので、評価方法の ID（`method_id`）は変わりません。足す前のレポートとも、共通の指標で比べられます。小さい `k` での回収は、`metric.evidence_recall` の `at` に `1` や `3` を足して測ります。

**段階ごとの回収。** レポートの `stages` に、毎回出ます。

| 項目 | 意味 |
| --- | --- |
| `stages.recall` の `retrieval.keyword` など | その検索モジュールが返した候補が覆った根拠の割合。同じモジュールを2回使うと `#2` が付きます |
| `stages.recall.retrieved` | どれかの検索が返した候補が覆った割合 |
| `stages.recall.candidates` | 融合と並べ替えの後、文脈の組み立てに渡った候補が覆った割合 |
| `stages.recall.context` | 文脈が覆った割合 |
| `stages.lost` | 主な回収で見つからなかった根拠の数を、理由ごとに数えたもの |

`stages.lost` の理由で、次に打つ手が分かれます。

| 理由 | 起きたこと | 打つ手の例 |
| --- | --- | --- |
| `not_retrieved` | どの検索の候補にも入らなかった | 辞書、ベクトル検索の併用、`top_k` を増やす、チャンクの切り方 |
| `dropped_by_fusion` | 検索の候補にはあったが、融合で候補の上限から外れた | 融合の `weight`、`max_candidates` |
| `ranked_low` | 候補にはあったが、上位 k 件（主な回収が `@k` のとき）や文脈の対象に入る順位まで上がらなかった | リランカー、融合の `weight` |
| `over_budget` | 候補にはあったが、文脈の予算がその順位に届く前に尽きた | リランカー、融合の `weight`、`budget_chars` を増やす |
| `truncated` | 文脈に一部だけ入った | `budget_chars`、`expand_lines`、チャンクを小さくする |
| `unresolved` | 引用を文書の中で特定できなかった | 評価データの引用を直す |

各行の `stages` には、根拠ごとの候補の中の順位（`rank`）と、失った理由（`lost`）が根拠の順に入ります。

**比較の確からしさ。** `report compare` の `scores` の各指標には、質問の組でのブートストラップによる95%区間（`interval_95`。グループが5つ以上のとき）と、符号検定の p 値（`sign_test_p`）が付きます。符号検定は、良くなった質問と悪くなった質問の数（`wins` と `losses`）だけを使い、同じ値の質問は数えません。値が小さいほど、差が偶然とは考えにくいことを示します。差のある質問が1つもなければ `null` です。

## 辞書候補を選ぶ

```sh
nest dictionary suggest --report baseline
```

標準辞書では、問い合わせの「東京大学」は1語ですが、正解の引用では「東京」「大学」に分かれています。`suggest` は追加候補の CSV 行と、語全体・部分語の確認用問い合わせを表示します。

```text
東京大学,名詞,トウキョウダイガク
  docs:product.txt — 製品東京大学は限定品
  質問の分かち書き 東京大学
  根拠の分かち書き 東京 / 大学
  語全体の質問 東京大学
  部分語の質問 大学
```

出力中の `東京大学,名詞,トウキョウダイガク` を、見出しなしの UTF-8 CSV として `selected.csv` に保存します。本体と同じディレクトリに `KnowledgeNestfile.yaml` のコピーを作り、候補設定に辞書を追加します。

```sh
cp KnowledgeNestfile.yaml candidate.yaml
nest dictionary add selected.csv --config candidate.yaml
```

`dictionary add` は、候補設定の `index.keyword` の `with.user_dictionary` に辞書のパスを設定します。

## 同じ評価データで試行と比較を行う

基準ビルドの抽出・チャンク化の結果を `--input baseline-build` で指定し、`--from index` で索引から作り直します。`dict` は試行ビルド、`dict-eval` はその評価レポートのタグです。

```sh
nest build --from index --input baseline-build --config candidate.yaml --tag dict
nest eval --build dict --tag dict-eval
nest report compare baseline dict-eval
```

比較結果の `comparable` が `true` であることを確認します。この例では `cases` の「東京大学」が `before: 0`、`after: 1`、`status: recovered` になり、辞書を追加すると登録した根拠を回収できることが分かります。

## 語全体と部分語の順位を確かめる

評価に登録した語全体の「東京大学」に加え、`suggest` が出した部分語の「大学」でも順位を調べます。両方のビルドで上位1件を取り、JSON の `data.results` で `document_id` が `docs:product.txt` の結果とその `rank` を見ます。

```sh
nest search "東京大学" --build baseline-build --limit 1 --json
nest search "東京大学" --build dict --limit 1 --json
nest search "大学" --build baseline-build --limit 1 --json
nest search "大学" --build dict --limit 1 --json
```

| 問い合わせ | 基準 `baseline-build` | 辞書を追加した試行 `dict` |
| --- | --- | --- |
| 東京大学 | 上位1件の圏外 | 1位 |
| 大学 | 1位 | 上位1件の圏外 |
