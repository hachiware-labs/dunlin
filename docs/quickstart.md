# サンプル資料で始める

CLI をインストールし、リポジトリのルートから始めます。4種別とも [ポンプの保全手順](../examples/manuals/maintenance.md) を使います。各コード例は、別々にリポジトリのルートから実行します。 [インストール](../README.md#インストール)

## MCP で資料を検索する

```sh
dunlin init mcp ./mcp-demo --source ./examples/manuals --no-embedding --no-ocr
cd mcp-demo
dunlin build
dunlin search "ポンプの点検はいつ実施しますか"
dunlin serve --print-config
```

検索結果に「ポンプの点検は毎月最初の営業日に行う。」と出典が出ます。最後のコマンドで表示した `command` と `args` を MCP クライアントへ登録すると、クライアントが検索を起動します。 [MCP クライアントの設定](mcp.md#mcp-で検索を提供する)

## RAG で出典付きの回答を得る

RAG、Wiki、PARA の例では、起動済みのローカル Ollama に `qwen3:4b` を用意します。

```sh
dunlin init rag ./rag-demo --source ./examples/manuals --llm-provider ollama --llm-model qwen3:4b --no-embedding --no-ocr
cd rag-demo
dunlin build
dunlin ask "ポンプの点検はいつ実施しますか"
dunlin serve
```

点検日と `maintenance.md` への出典が回答に含まれることを確かめます。HTTP では `127.0.0.1:4318` の `/v1/responses` に質問、`/v1/search` に検索語を送れます。 [RAG の起動とリクエスト例](mcp.md#rag-で回答と検索を提供する)

## Wiki にページを作る

```sh
dunlin init wiki ./wiki-demo --purpose "ポンプの保全手順をまとめる" --llm-provider ollama --llm-model qwen3:4b --no-ocr
cd wiki-demo
dunlin ingress ../examples/manuals
dunlin serve
```

ブラウザで `http://127.0.0.1:4319` を開き、左の一覧から生成したページを選びます。本文の点検日と出典を確認します。ページ名はモデルの生成結果に応じて変わります。 [Wiki・PARA の操作](wiki.md)

## PARA に資料を整理する

Wiki の `serve` を使っている場合は `Ctrl+C` で止めてから、リポジトリのルートで始めます。Wiki と PARA の既定ポートは共に `4319` です。

```sh
dunlin init para ./para-demo --purpose "設備保全に関する資料を整理する" --llm-provider ollama --llm-model qwen3:4b --no-ocr
cd para-demo
dunlin ingress ../examples/manuals
dunlin serve
```

同じ URL で Projects・Areas・Resources・Archives の分類を開き、資料から作られたノートとその出典を確認します。 [Wiki・PARA の操作](wiki.md)
