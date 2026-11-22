# 検索を評価し、ユーザー辞書を試す

評価データに質問と正解の引用を1件登録します。基準の索引でその根拠を拾えなければ、ユーザー辞書を足した試行を作り、同じ質問で評価し直します。最後に、語全体と部分語で同じ文書の順位を確認します。

## 資料を用意する

CLI をインストールし、リポジトリのルートで新しい MCP プロジェクトを作ります。この例は同梱辞書を使うキーワード検索で進めます。 [CLI のインストール](../README.md#インストール)

```sh
knowledge-nest init mcp ./evaluation-demo
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
knowledge-nest build
knowledge-nest eval-data add --question "東京大学" --doc "docs:product.txt" --quote "製品東京大学は限定品"
knowledge-nest eval --tag baseline
knowledge-nest report show baseline
```

この例では `scores` の `evidence_recall@context` が `0` になります。登録した引用が、実際に返す検索文脈に含まれなかったことを示しています。

現在提供中の基準ビルドにもタグを付けます。

```sh
knowledge-nest tag baseline-build
```

## 質問と引用の候補をモデルに作らせる

質問と引用を手で書く代わりに、設定済みの対話用モデルに候補を作らせることができます。`KnowledgeNestfile.yaml` の `models` に `kind: chat` のモデルが1つだけあれば `--model` は省けます。

```sh
knowledge-nest eval-data generate --count 30
```

- 評価ケースがまだない文書から先に読みます。長い文書は、ケースの引用がまだ少ない部分から、文書の中に散らばる順に読みます。`--doc` で対象を1文書に絞れます。
- 候補には質問の種類が付きます。`lexical` は引用の語を使う質問、`paraphrase` は言い換えた質問、`identifier` は型番・数値・名前で尋ねる質問です。結果の `kinds` に種類ごとの件数が出ます。`--kind paraphrase` のように指定すると、その種類だけを作ります。
- 1つの引用で答えられない質問は、3つまでの引用を持ちます。引用は別々の文書にあってもかまいません。
- 引用が原文にない、文書内で一意に決まらない、300文字を超える、質問が重複している候補は採用しません。結果の `dropped` に理由ごとの件数が出ます。
- 作られたケースの状態は `generated` で、確認するまで評価の対象になりません。モデルを使うため、読ませた範囲のテキストはそのモデルの提供元へ送られます。

候補を一覧し、妥当なものを採用します。`--case` は繰り返して指定できます。

```sh
knowledge-nest eval-data review <dataset>
knowledge-nest eval-data review <dataset> --case <id> --case <id> --state reviewed --reviewer <name> --reviewer-type human
```

`<dataset>` は `generate` が表示した評価データの ID です。採用しない候補は `--state rejected` にします。まとめて編集したい場合は `eval-data export` で書き出し、`eval-data import` で戻せます。

## 辞書候補を選ぶ

```sh
knowledge-nest dictionary suggest --report baseline
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
knowledge-nest dictionary add selected.csv --config candidate.yaml
```

`dictionary add` は、候補設定の `index.keyword` の `with.user_dictionary` に辞書のパスを設定します。

## 同じ評価データで試行と比較を行う

基準ビルドの抽出・チャンク化の結果を `--input baseline-build` で指定し、`--from index` で索引から作り直します。`dict` は試行ビルド、`dict-eval` はその評価レポートのタグです。

```sh
knowledge-nest build --from index --input baseline-build --config candidate.yaml --tag dict
knowledge-nest eval --build dict --tag dict-eval
knowledge-nest report compare baseline dict-eval
```

比較結果の `comparable` が `true` であることを確認します。この例では `cases` の「東京大学」が `before: 0`、`after: 1`、`status: recovered` になり、辞書を追加すると登録した根拠を回収できることが分かります。

## 語全体と部分語の順位を確かめる

評価に登録した語全体の「東京大学」に加え、`suggest` が出した部分語の「大学」でも順位を調べます。両方のビルドで上位1件を取り、JSON の `data.results` で `document_id` が `docs:product.txt` の結果とその `rank` を見ます。

```sh
knowledge-nest search "東京大学" --build baseline-build --limit 1 --json
knowledge-nest search "東京大学" --build dict --limit 1 --json
knowledge-nest search "大学" --build baseline-build --limit 1 --json
knowledge-nest search "大学" --build dict --limit 1 --json
```

| 問い合わせ | 基準 `baseline-build` | 辞書を追加した試行 `dict` |
| --- | --- | --- |
| 東京大学 | 上位1件の圏外 | 1位 |
| 大学 | 1位 | 上位1件の圏外 |
