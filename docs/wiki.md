# Wiki・PARA の使い方

Wiki は資料を引用したページを作ります。PARA は同じ操作で、Projects・Areas・Resources・Archives に沿ってノートを整理します。

## 最初のページを作る

リポジトリのルートで、ローカル Ollama を起動し、`qwen3:4b` を用意します。例では [サンプル資料](../examples/manuals/maintenance.md) を使います。

```sh
dunlin init wiki ./wiki-demo --purpose "設備保全の手順をまとめる" --llm-provider ollama --llm-model qwen3:4b --no-ocr
cd wiki-demo
dunlin ingress ../examples/manuals
dunlin serve
```

PARA を作る場合は `init para ./para-demo` に読み替えます。ブラウザで `http://127.0.0.1:4319` を開きます。左に原本とページの一覧、中央に本文と出典、右に質問と操作があります。生成した HTML はプロジェクト内の `site/` に保存されます。

## 画面から資料を追加する

1. 右側の **資料ファイル** でローカルのファイルを選び、**資料を追加** を押します。
2. 作業ページで読み取り・執筆・引用検証の進捗を確認します。別の画面へ移動した場合は **最近の作業** から開き直せます。
3. 完了後、変更されたページを開き、本文と出典を見比べます。

CLI の `ingress` も作業 ID を表示します。別の端末でプロジェクトを開き、`WORK_ID` を表示された ID に置き換えると進捗を確認できます。

```sh
dunlin progress WORK_ID --json
```

## 質問し、ページを直す

右側に質問を書いて **質問する** を押します。ページを開いているときは、そのページや選択した範囲を指定できます。ページへの変更を依頼する場合は、対象ページで指示を書き、**変更・出典** の **ページへの指示を実行** を使います。補足情報を根拠にする場合は、その内容と記録者も入力します。

CLI では以下の `PAGE_ID` を対象の ID に置き換えます。`cite` に渡す番号は、ページ内の引用番号です。

```sh
dunlin query "ポンプの点検はいつですか" --page PAGE_ID
dunlin cite PAGE_ID 1
dunlin instruct PAGE_ID "点検前の確認事項を箇条書きにしてください"
```

今後の作業にも使う決まりは **今後の決まりに追記** または `dunlin instruct --persistent` で登録します。現在の内容は左側の **運用の決まり** から確認できます。

## 検査して、必要な変更を取り消す

**検査** は引用、原本の版、リンクなどを確認します。**内容を検査** はモデルを使い、変更したページと関連ページの食い違いを調べます。指摘に表示された引用と出典を開いて確認します。

```sh
dunlin verify
dunlin verify --content
```

変更を戻すには **最近の作業** で対象の作業を開き、**取り消す** が表示されていれば押します。CLI では同じ作業 ID を `dunlin undo WORK_ID` に渡します。ページをアーカイブから戻す操作は **復元** または `dunlin archive PAGE_ID --restore` です。
