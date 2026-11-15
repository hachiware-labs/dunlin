# Dunlin

**手元の資料フォルダを、3つのコマンドで「AI エージェントが引ける知識」に変えるツールです。**

例では、ポンプの点検手順を含む Markdown やテキストを `./manuals` に置いて検索します。

```sh
dunlin init mcp ./kb --source ./manuals --no-embedding --no-ocr
cd kb
dunlin build
dunlin search "ポンプの点検はいつ実施しますか"
```

キーワード検索は手元で完結します。MCP クライアントに `dunlin serve` を登録すると、エージェントからも同じ検索と出典を利用できます。

## 何ができるか

Dunlin は、資料の取り込みから検索、提供、評価、改善までを1つのパッケージで行います。作るものを最初に1つ選びます。

| 作るもの | 選ぶ種別 | できること |
|---|---|---|
| エージェント用の検索 | `init mcp` | 資料を検索し、出典付きの文脈を MCP で返す |
| 質問に答える API | `init rag` | 検索した文脈から、出典付きの回答を生成する。OpenAI の Responses API 形式で返す |
| AI が書く Wiki | `init wiki` | 資料を読んで、引用付きのページを AI が作り、保守する |
| AI が整理するノート | `init para` | Projects・Areas・Resources・Archives の形でノートを管理する |

## 特徴

- **すぐ動く**。標準のキーワード検索を日本語の自然文で使えます。
- **入れれば足りる**。文書の抽出、索引、検索、サーバー、評価が同梱されています。
- **構成を設定ファイルで確認できる**。初期構成のモジュールと引数は `Dunlinfile.yaml` に書き出されます。
- **測って改善できる**。質問と正解の引用を登録し、検索が根拠を拾えているかを測ります。設定の候補を試し、比べてから採用できます。
- **文書の更新に追従する**。同じ設定で資料を更新したら `dunlin build` を再実行します。変更した文書の処理結果を新しい索引に反映し、稼働中の検索サーバーは次の要求から使います。
- **資料と索引を手元に置く**。HTTP サーバーをループバックの外で待ち受ける場合は認証が必須です。

対応する資料は、Markdown、UTF-8 テキスト、HTML、PDF、Word（`.docx`）、Excel（`.xlsx`）、PowerPoint（`.pptx`）です。画像やスキャンした PDF は、OCR を追加すると読めます。

## インストール

npm への公開は準備中です。現在はソースからビルドします。Rust 1.94.1 と、Node.js 22.20 以降の 22 系または 24.12 以降の 24 系が必要です。

```sh
git clone https://github.com/hachiware-labs/dunlin
cd dunlin
npm ci
npm run build
npm link
dunlin --help
```

## 使い方

各例は `./manuals` のあるディレクトリから始めます。

### エージェント用の検索を作る（mcp）

冒頭の手順を実行済みなら、`kb` 内で `dunlin serve --print-config` に進みます。

```sh
dunlin init mcp ./kb --source ./manuals --no-embedding --no-ocr
cd kb
dunlin build
dunlin search "手順"
dunlin serve --print-config
```

最後のコマンドが出力する内容を、MCP クライアントの設定に登録します。資料を更新したら `dunlin build` を再実行します。

対応していない形式のファイルがあると、`build` は一覧を表示して止まります。`dunlin build --skip-unsupported` で飛ばして進められます。

### 質問に答える API を作る（rag）

この例では、起動済みの Ollama に `qwen3:4b` を用意します。

```sh
dunlin config set llm --provider ollama --model qwen3:4b   # 使うモデルを一度だけ登録
dunlin init rag ./rag --source ./manuals
cd rag
dunlin build
dunlin ask "ポンプの点検周期は？"
dunlin serve
```

`dunlin serve` は、1つのポート（`127.0.0.1:4318`）で3つの口を出します。

| 口 | 内容 |
|---|---|
| `POST /v1/responses` | 出典付きの回答を OpenAI の Responses API 形式で返す |
| `POST /v1/search` | 検索結果と文脈を JSON で返す |
| `/mcp` | エージェントに検索結果と出典を渡す |

### AI が書く Wiki を作る（wiki / para）

この例も Ollama の `qwen3:4b` を使います。PARA 形式で整理する場合は `init para ./para` を使います。

```sh
dunlin config set llm --provider ollama --model qwen3:4b
dunlin init wiki ./wiki --purpose "設備保全の手順と部品情報をまとめる"
cd wiki
dunlin ingress ../manuals
dunlin serve
```

ブラウザで `http://127.0.0.1:4319` を開くと、左に資料の階層、中央にページ、右に質問と操作の欄が並びます。

- ページの記述には、原文の該当箇所への引用が付きます。引用を検証できない記述は保存されません。
- 資料の原本は書き換えません。変更は作業単位で記録され、取り消せます。
- `dunlin verify` でリンク切れや古い引用を検査します。`dunlin verify --content` で、ページ間の内容の食い違いも検査します。

## 検索を改善する

検索が外れると感じたら、構築済みの MCP または RAG プロジェクトで質問と正解の引用を登録して測ります。この例では `保全手順.md` に引用と同じ文があるものとします。

```sh
# 1. 質問と、答えが書かれている文書・引用を登録する
dunlin eval-data add --question "シールの交換周期は" --doc "保全手順.md" --quote "メカニカルシールは2年ごとに交換する"

# 2. 今の設定を測る
dunlin eval --tag baseline

# 3. candidate.yaml の chunk.length の with.size を編集して試す
cp Dunlinfile.yaml candidate.yaml
# 例として size を 800 から 400 に変更してから実行する
dunlin build --from transform --config candidate.yaml --tag trial
dunlin eval --build trial --tag trial-eval

# 4. 評価レポートを比べる
dunlin report compare baseline trial-eval
```

比較結果を見て採用する場合は、試行ビルドのタグを指定します。

```sh
dunlin use trial
```

稼働中のサーバーに採用した設定を反映するには、`serve` を起動し直します。

試行では抽出済みの資料を再利用し、変更した段階から処理します。評価で外れた質問からユーザー辞書の候補を調べるには `dunlin dictionary suggest --report baseline` を実行します。 [辞書の候補と試行](docs/dunlin-spec.md#ユーザー辞書の候補を提案する)

## 設定ファイル

`init` は、最初に使うモジュールと引数を `Dunlinfile.yaml` に書き出します。

```yaml
transform:
  modules:
    - use: text.normalize
      with:
        unicode: nfkc
    - use: chunk.length
      with:
        unit: characters
        size: 800
        overlap: 100
```

- `dunlin modules list` で、使える部品と引数を確認できます。
- `dunlin validate` で、設定を実行前に検査できます。名前や引数の誤りの場所と修正の手がかりを表示します。
- `dunlin add --list` で、追加できる道具（OCR、近似ベクトル索引）を確認できます。

## ドキュメント

| 文書 | 内容 |
|---|---|
| [MCP と RAG の起動](docs/mcp.md) | 起動手順、モデルの設定、日本語検索とユーザー辞書、接続制限 |
| [Node.js API](docs/api.md) | 索引を直接操作するライブラリとしての使い方 |
| [受け入れ確認](docs/acceptance.md) | 仕様の受け入れ条件に対する確認結果 |
| [設計仕様](docs/dunlin-spec.md) | コマンド、設定、評価、Wiki/PARA の仕様の全体 |

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

`test:package` は `dist/packages/` に tarball を作り、空のプロジェクトへオフラインでインストールして検索と本文取得を試します。ベンチマークは合成データで索引作成、検索レイテンシ、メモリ、Recall@10、nDCG@10 などを計測します。

CI は手動で実行します。GitHub Actions の **Verify Dunlin** を起動すると、Windows と Ubuntu を検証します。macOS の arm64・x64 を検証するには **Run only the macOS ARM and Intel checks** を選びます。

## 現在の状態

- バージョン 0.5.0 の開発版です。コマンドや設定の形は変わる可能性があります。
- `dunlin` は仮のパッケージ名です。npm への公開とライセンスは未定です。
- 動作を確認している環境は、Windows x64、Linux x64（glibc）、macOS arm64 と x64 です。
