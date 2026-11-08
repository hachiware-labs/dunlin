# Dunlin CLI リファレンス

資料の取り込み、RAG のモデル・ベクトル保存先、文書パーサー、MCP と Web 閲覧の設定を説明します。初めて使うときは、目的別ガイドのサンプル資料で動作を確認できます。[PARA](../../docs/para.md) · [LLM Wiki](../../docs/llm-wiki.md) · [RAG](../../docs/rag.md) · [MCP](../../docs/mcp.md)

## セットアップと取り込み

[CLI のセットアップ](../../README.md#cli-のセットアップ)を済ませ、リポジトリのルートから実行します。

```sh
dunlin ingest --index ./knowledge --source local ./my-docs
dunlin ingest --index ./knowledge --source handbook s3://my-bucket/handbook/
```

`./my-docs` は資料が入っているフォルダに置き換えます。ローカル文書は Dunlin のファイル登録を使います。HTTP(S)・S3 の取り込みと MCP の `source.upload` には `tools/import` の依存関係が必要です。同じソース ID で再実行するとそのソースを一括更新します。[S3 の設定と対応形式](../import/README.md)

## ディレクトリごとの設定

セットアップ後、`dunlin` コマンドを任意の作業ディレクトリから使えます。

```sh
dunlin init --preset rag
dunlin config show
dunlin config set sources.docs ./handbook
dunlin config set rag.vectorStore.type sqlite
dunlin ingest
dunlin embed
dunlin ask "バックアップの保管期間は？"
```

端末で引数なしの `dunlin init` を実行すると、PARA・LLM Wiki・RAG・検索専用 MCP から用途を選べます。表示された作業フォルダでよければ Enter を押します。`n` または Ctrl+C で中止できます。用途や場所を直接指定するには `dunlin init ./wiki-work --preset llm-wiki` のように実行します。パイプなどから引数なしで実行すると、現在のフォルダに既定の `rag` プリセットを作ります。

`init` は対象フォルダに `dunlin.json` と `docs/` を作ります。標準出力には設定ファイルの絶対パス、標準エラーには対話の案内とプリセットに合わせた次の手順を表示します。`rag` プリセットは Ollama の回答・埋め込みモデルと SQLite を設定します。`mcp` は共通の索引と取り込み元を設定します。`para` と `llm-wiki` は編集用 MCP の保存先も設定します。

- `config show` は保存されている設定を表示します。`config set <key> <value>` は文字列または JSON の値を受け取り、変更後の設定全体を検証して保存します。たとえば `dunlin config set rag.embedding '{"provider":"openai","model":"text-embedding-3-small"}'` でプロバイダーとモデルを一緒に変更できます。設定ファイルは直接編集することもできます。
- 各コマンドは実行ディレクトリから親へたどり、最も近い `dunlin.json` を読みます。`--config <file>` で別のファイルを指定できます。コーパスや索引を置く `index` とローカル文書の相対パスは設定ファイルの場所が基準です。CLI の明示フラグを優先します。プロバイダーをフラグで切り替える場合は、対応するモデルも明示します。
- `ingest` は設定したすべてのソースをソース単位で更新します。`ingest --source docs` は指定したソースだけを更新します。`s3Endpoint` で S3 互換エンドポイントを指定できます。API キーと AWS 認証情報は環境変数やプロバイダーの認証設定を使います。
- 回答モデルの変更は次の `ask` に反映します。埋め込みモデルや保存先を切り替え、その設定に合うベクトル索引がない場合は `embed` を実行します。`config set rag.embedding null` で `ask` をキーワード検索に切り替えられます。

## 文書コーパスと原本の設定

ローカルファイル、HTTP(S)、S3、MCP から取得した原本をパースし、全文を文書ごと・版ごとにコーパスへ保存します。コーパスの保存先は `index` 配下の `.dunlin-corpus-v1` です。各版には出典 URL、パーサー情報、全文の行範囲と位置メタデータを記録します。パーサーがページ・座標・シンボルなどを返した場合は、該当する行範囲にひも付けます。[用語と取り込みの流れ](../../README.md#文書コーパスと原本)

`ingest` は全文をコーパスに保存し、キーワード索引用に最大32行のチャンクを作ります。`embed` は同じ版の全文をベクトル検索用にチャンキングし、各チャンクの埋め込みをベクトル索引に保存します。

取得時の原本は `.dunlin-uploads-v1` に保存します。パーサーで取り込んだ PDF・Office・画像と `source.upload` の添付は既定で保存します。テキストや HTML は、原本も保管するソース ID に `keepOriginals` を設定します。CLI で個別に取り込む場合は `--keep-original` も指定できます。

```sh
dunlin config set keepOriginals.docs true
dunlin ingest --source docs
dunlin ingest --source policy --keep-original https://example.com/policy.html
```

`source.ingest` では `keepOriginal: true` を渡せます。継続して同じ方針を使う場合は、取り込み結果の `document.sourceId` を `keepOriginals` に設定し、MCP に接続し直します。`false` は、その後のテキストや HTML の取り込みで原本保存を省きます。保存済みの版と原本は引き続き参照できます。

`source.read` は `version` を省くと現在の版の本文を読み、指定するとコーパスに保存したその版の本文を読みます。PARA と LLM Wiki の出典画面は、コーパスに保存した引用時の版を使います。コーパス導入前の文書は現在の索引から読み出せます。過去版はコーパスに保存した版から利用できます。

### コーパスの名称とメタデータ

`dunlin.json` のあるプロジェクトで、コーパス全体の名称と説明を設定できます。`metadata.<キー>` には担当チームやタグなど、用途に合わせた項目を追加できます。

```sh
dunlin corpus set name "バックアップ運用"
dunlin corpus set description "保管方針と復旧手順を集めた文書群"
dunlin corpus set metadata.team "インフラ運用"
dunlin corpus show
```

`name` と `description` は文字列です。任意のメタデータには文字列や JSON の値を指定できます。設定は `<index>/.dunlin-corpus-v1/manifest.json` に保存します。別の設定ファイルから同じ `index` を参照すると、同じ情報が表示されます。対象を指定する場合は `--config <file>` または `--index <directory>` を使えます。

コーパスの名称やメタデータを編集しても、文書の版や索引の更新は発生しません。

### コーパスと検索の状態

`dunlin corpus show` はコーパスの名称やメタデータとともに、`status` に参照中の設定ファイル `configPath`、保存先 `indexPath`、ソース別の文書数 `sources` を JSON で表示します。設定にある未取り込みのソースも文書数 `0` で表示します。MCP などから個別に取り込んだソースは `configured: false` で表示します。数えるのは現在の索引に含まれる文書です。

| 項目 | 状態と意味 |
|---|---|
| `search.keyword.state` | `not_indexed` は索引の作成前、`empty` はチャンクのある文書が0件、`ready` はチャンクのある文書が1件以上 |
| `search.vector.state` | `not_configured` は埋め込み未設定、`needs_keyword_index` は文書の取り込みが必要、`missing` は現在の埋め込み設定で索引作成が必要、`stale` はキーワード索引更新後に再作成が必要 |
| `search.vector.state` | `empty` は埋め込みが0件、`ready` は保存済みのベクトル索引が現在のキーワード索引と一致 |

索引や埋め込み設定を読み取れない場合は、その検索方式に `unavailable` と理由を表示します。キーワード索引が読み取れないときは、文書数に `null` を表示します。判定にはローカルの設定と索引の保存状態を使います。

### 環境変数を使う

`dunlin.json` のパス、プロバイダー、モデル名、接続先などの文字列に `${NAME}` 形式で環境変数を指定できます。たとえば `"./${DUNLIN_INDEX_DIR}"` は、展開後も相対パスなら設定ファイルから解決します。

```json
{
  "version": 1,
  "index": "./${DUNLIN_INDEX_DIR}",
  "sources": { "docs": "${DUNLIN_DOCS_PATH}" },
  "rag": {
    "generation": {
      "provider": "openai",
      "model": "${DUNLIN_LLM_MODEL}",
      "baseUrl": "${DUNLIN_LLM_URL}",
      "apiKeyEnv": "DUNLIN_LLM_KEY"
    }
  }
}
```

`apiKeyEnv` は `rag.generation`、`rag.embedding`、Chroma または Qdrant の `rag.vectorStore` に設定できます。値には環境変数名を指定します。省略時は `OPENAI_API_KEY` など、各プロバイダーに既定の変数があればその変数を使います。

`config show` と `config set` は参照式をそのまま扱います。シェルから参照式を登録する場合は `dunlin config set rag.generation.baseUrl '${DUNLIN_LLM_URL}'` のように囲います。`ingest`・`embed`・`ask` は実行時に必要な値を展開し、未設定なら環境変数名と設定項目を表示します。明示した CLI フラグが設定値より優先されます。

## 質問する

```sh
dunlin ask --index ./knowledge --provider openai --model gpt-4o-mini "バックアップの保管期間は？"
dunlin ask --index ./knowledge --provider ollama --model qwen3:4b "バックアップの保管期間は？"
dunlin ask --index ./knowledge --provider strata --model strata "バックアップの保管期間は？"
```

| プロバイダー | 設定 |
|---|---|
| OpenAI | `OPENAI_API_KEY` を設定し、構造化出力に対応するモデルを指定します。既定のベース URL は `https://api.openai.com/v1` です。 [OpenAI Docs の構造化出力](https://developers.openai.com/api/docs/guides/structured-outputs) |
| Ollama | 起動済みの Ollama とインストール済みのモデルを指定します。既定のベース URL は `http://127.0.0.1:11434` です。 [Ollama チャット API](https://docs.ollama.com/api/chat) |
| Strata | 起動済みの Strata サーバーと公開されているモデル名を指定します。既定のベース URL は `http://127.0.0.1:8080/v1` です。サーバーに認証を設定した場合は `STRATA_API_KEY` を設定します。構造化出力に対応するサーバーが必要です。 |

`--base-url` で接続先を指定できます。環境変数ならそれぞれ `OPENAI_BASE_URL`、`OLLAMA_BASE_URL`、`STRATA_BASE_URL` です。`--source local --source handbook` で検索対象を指定し、`--query "保管期間 バックアップ"` で質問と別の検索語を渡せます。`--json` は回答と引用を JSON で出力します。

質問と検索で得たチャンクの本文を、回答モデルの接続先に送ります。最大8件の検索結果から、行番号付きの抜粋を最大16 KiB 送ります。出典 URL と引用本文は、現在の索引が参照する文書の版から確定します。モデルが返した引用 ID や行範囲が取得済みの根拠を外れていればエラーにし、根拠不足の場合はその旨を表示します。

## ベクトル検索

取り込み済みの文書からベクトル索引を作り、質問時に同じ埋め込みプロバイダーとモデルを指定します。回答生成のプロバイダーは別に選べます。

```sh
dunlin embed --index ./knowledge --embedding-provider ollama --embedding-model embeddinggemma
dunlin ask --index ./knowledge --provider ollama --model qwen3:4b --embedding-provider ollama --embedding-model embeddinggemma "復旧にはどれくらいかかる？"

dunlin embed --index ./knowledge --embedding-provider openai --embedding-model text-embedding-3-small
dunlin ask --index ./knowledge --provider ollama --model qwen3:4b --embedding-provider openai --embedding-model text-embedding-3-small "復旧にはどれくらいかかる？"
```

- Ollama には指定した埋め込みモデルを用意します。OpenAI は `OPENAI_API_KEY` を使います。埋め込み先は `--embedding-base-url` で上書きでき、省略時はプロバイダーごとの `OLLAMA_BASE_URL` または `OPENAI_BASE_URL` と既定値を使います。 [Ollama Embed API](https://docs.ollama.com/api/embed) · [OpenAI Docs の Embeddings API](https://developers.openai.com/api/reference/resources/embeddings/methods/create)
- `embed` は現在の索引が参照する版の全文から、最大16行・2 KiB のチャンクを作って埋め込み先に送信します。質問時は質問の埋め込みと各チャンクの埋め込みをコサイン類似度で比較します。出典の行番号はコーパスの全文に対応します。
- 索引のリビジョンや埋め込みモデルの中身が変わったら `embed` を再実行します。`ask` は CLI と設定ファイルの両方で埋め込みの指定が省略されている場合、キーワード検索を使います。

### ベクトル索引の保存先

`embed` と `ask` で同じ `--vector-store` と接続先を指定します。埋め込みプロバイダーとは独立に選べます。

| 値 | 保存先 | 設定 |
|---|---|---|
| `local`（既定） | `index` 配下の JSON | サーバー不要 |
| `sqlite` | `index` 配下の SQLite。`sqlite-vec` でコサイン距離を計算 | サーバー不要。`npm --prefix tools/rag ci` で対応プラットフォーム用の拡張を入れます。 [sqlite-vec の Node.js 対応](https://alexgarcia.xyz/sqlite-vec/js.html) |
| `chroma` | Chroma の v2 API | 既定は `http://127.0.0.1:8000`。`CHROMA_URL`、`CHROMA_TENANT`、`CHROMA_DATABASE`、`CHROMA_API_KEY` を設定できます。 [Chroma API](https://docs.trychroma.com/reference/chroma-api/collection/create-collection) |
| `qdrant` | Qdrant の REST API | 既定は `http://127.0.0.1:6333`。`QDRANT_URL`、`QDRANT_API_KEY` を設定できます。 [Qdrant API](https://api.qdrant.tech/api-reference/search/query-points) |

```sh
dunlin embed --index ./knowledge --embedding-provider ollama --embedding-model embeddinggemma --vector-store sqlite
dunlin ask --index ./knowledge --provider ollama --model qwen3:4b --embedding-provider ollama --embedding-model embeddinggemma --vector-store sqlite "復旧にはどれくらいかかる？"
```

Chroma や Qdrant の接続先は `--vector-store-url` でも指定できます。サーバーにはベクトル、ポイント ID、絞り込み用のソース ID を保存し、引用情報はローカルに保持します。再構築は新しいコレクションへ書き込み、完了時に切り替えます。古いコレクションを整理する場合は、利用中の検索が終わってから DB の管理操作で削除します。

## 検証

```sh
npm --prefix tools/rag test
```

ローカルフォルダと S3 の模擬エンドポイントから取り込み、回答生成と引用の組み立てを検証します。設定のテストでは初期化、無効な変更の拒否、設定変更後の取り込み・埋め込み・回答への反映を確認します。埋め込みのテストでは両 API のリクエスト形式、キーワード一致のない質問での取得、索引更新後の古いベクトルの検出を確認します。

## 文書パーサーをセットアップする

`npm --prefix tools/rag ci` で対応するプラットフォームのパッケージを入れます。対象プロジェクトで `dunlin init` を実行してから、使うカテゴリをセットアップします。

```sh
dunlin setup documents
dunlin setup documents --tool docling-serve --base-url http://127.0.0.1:5001
dunlin setup ocr --base-url http://127.0.0.1:8010/v1
dunlin setup ocr --tool tesseract-js --lang ja
dunlin setup code
dunlin setup --check
dunlin setup ocr --check
```

| カテゴリ | 既定ツール | 処理と準備 |
|---|---|---|
| `documents` | `docling-rs` | DOCX、XLSX、PPTX と PDF を取り込み時にローカル変換します。セットアップは Docling の公開チェックサムを照合し、PDF のレイアウト・表解析モデルを `.dunlin-models/docling` に保存します。`docling-serve` は共有先を優先し、接続できない場合にローカルで変換します。 |
| `ocr` | `lightonocr` | GPU 用の Python 環境と LightOnOCR-3-1B を準備します。外部の OpenAI 互換サービスを使う場合は `--base-url` を指定します。`tesseract-js` は npm の言語データで日本語の `ja` と縦書きの `ja-vert` を選べます。 |
| `code` | `ast-grep` | JavaScript、TypeScript、TSX、Python、Rust の定義を取り出し、元の行範囲とシンボル名を保持します。 |

`--tool` の指定、`dunlin.json` の `parsers` 設定、カテゴリの既定値の順で選びます。準備が成功すると設定を保存します。`--check` は確認専用で、利用可否と不足条件を JSON で返します。`--config` で対象プロジェクトを指定できます。サービス認証には `parsers.<カテゴリ>.apiKeyEnv` で環境変数名を設定します。ベース URL、モデル名、モデル保存先は `${ENV_VAR}` を使えます。

Windows x64、Linux x64 glibc、macOS Apple Silicon の `docling.rs` は npm のビルド済み版を使います。Intel Mac では `docling-serve` を選べます。 [Docling の配布対象](https://github.com/docling-project/docling.rs/blob/master/crates/docling-node/README.md#install) · [LightOnOCR のサーバー起動手順](https://github.com/lightonai/LightOnOCR#quick-start)

### Docling をローカルで使う・共有する

通常は対象プロジェクトで `dunlin setup documents` を実行し、`dunlin ingest --source report ./report.pdf` で取り込みます。既定の `docling-rs` は取り込み元のプロジェクト内で変換します。

同じ端末で変換サービスを共有するときは、セットアップ済みのディレクトリから `dunlin serve-docling` を実行します。既定の待受先は `127.0.0.1:5001` です。`--config` または `--model-dir` で共有側のモデルを指定できます。利用するプロジェクトごとに次を実行します。

```sh
dunlin setup documents --tool docling-serve --base-url http://127.0.0.1:5001
dunlin ingest --source report ./report.pdf
```

接続先が停止している場合は、取り込み元のプロジェクトの `parsers.documents.modelDir` を使ってローカルで変換します。ローカルのレイアウトモデルが用意されていない場合、PDF はテキスト層を抽出します。`dunlin setup documents --check` の `execution` で `shared` または `local` を確認できます。接続先から HTTP エラーや変換エラーが返った場合は、そのエラーが取り込み結果に反映されます。

### 取り込み時に LightOnOCR をローカル GPU で使う

```sh
dunlin setup ocr
dunlin ingest --source scan ./scan.pdf
```

初回の `setup ocr` はネットワーク接続と `tar` を使い、専用の Python 3.12 環境に GPU 用の PyTorch を入れます。モデルを含めて数 GB のダウンロードと空き容量が必要です。モデルは固定リビジョンから取得し、ファイルごとに SHA-256 を検証します。GPU で演算できることを確認した環境をユーザーのデータ領域に保存するため、複数プロジェクトで共有できます。既定の保存先は Windows が `%LOCALAPPDATA%\Dunlin\ocr`、Linux が `$XDG_DATA_HOME/dunlin/ocr` または `~/.local/share/dunlin/ocr`、macOS が `~/Library/Application Support/Dunlin/ocr` です。`DUNLIN_OCR_HOME` で絶対パスを指定できます。

| 環境 | 管理対象の GPU 実行環境 |
|---|---|
| Windows x64 | 対応 AMD GPU は ROCm 7.2.1、対応 NVIDIA GPU は CUDA 12.8 |
| Linux x64 | 対応 AMD GPU は ROCm 7.2.1、対応 NVIDIA GPU は CUDA 12.8 |
| macOS Apple Silicon | PyTorch MPS（Apple Metal） |

GPU ドライバーは事前に OS へ導入してください。Linux の AMD 環境では、対応ディストリビューションで ROCm のシステム側の準備も必要です。複数の GPU がある場合や既存の外部 OCR 設定をローカルに切り替える場合は、`dunlin setup ocr --backend rocm` のように指定できます。 [AMD Windows](https://rocm.docs.amd.com/projects/radeon-ryzen/en/latest/docs/install/installryz/windows/install-pytorch.html) · [AMD Linux](https://rocm.docs.amd.com/projects/radeon-ryzen/en/latest/docs/install/installryz/native_linux/install-pytorch.html) · [NVIDIA 向け PyTorch](https://pytorch.org/get-started/previous-versions/#v291) · [PyTorch MPS](https://docs.pytorch.org/docs/stable/notes/mps.html)

取り込みに使うプロジェクトでは、先に `dunlin init` で `dunlin.json` を作成してから `dunlin setup ocr` を実行します。セットアップに成功すると OCR 設定が保存されます。CLI と MCP は画像ページや画像ファイルに初めて到達したときに専用の GPU プロセスを起動し、処理中の文書で使い回します。処理がすべて終わると GPU プロセスを終了します。`dunlin setup ocr --check` の `prepared` はローカル環境の準備状況です。

### LightOnOCR サービスを共有する

複数のプロジェクトやクライアントで GPU に載せたモデルを共有するときは、`dunlin serve-ocr` で常駐サービスを起動します。サービスは `127.0.0.1:8010` で待ち受けます。利用するプロジェクトで `dunlin setup ocr --base-url http://127.0.0.1:8010/v1` を実行します。同じ方法で外部の互換サービスも指定でき、`dunlin setup ocr --check` の `serving` で疎通を確認できます。

共有先のモデルが `lightonai/LightOnOCR-3-1B` の場合は、接続できなくなると準備済みのローカル GPU で OCR を続行します。ローカル環境は共有先を設定する前に `dunlin setup ocr` で準備できます。`dunlin setup ocr --check` の `execution` には `shared` または `local` が表示されます。共有先から返った HTTP エラーや認識エラーは取り込み結果に反映されます。

ローカルの LightOnOCR は、CUDA または ROCm でネイティブ BF16 を使える GPU では BF16、その他の CUDA・ROCm GPU と Apple Metal では FP32 を使います。共有サービスの CPU 起動は `serve-ocr --device cpu` で指定でき、別の環境やモデルは `--python` と `--model-dir` で指定できます。

設定したパーサーは CLI のローカルファイル・フォルダ・HTTP(S)・S3 取り込みと、MCP の `source.ingest`・`source.upload` で使います。OCR を設定すると、PDF のテキスト層があるページはその本文を使い、画像ページは OCR に渡します。PNG、JPEG、WebP も OCR の対象です。変換対象の原本は最大 16 MiB、OCR 対象の PDF は最大 128 ページ、抽出本文は最大 1 MiB です。OCR の出典には原本のページと、取得できた場合は左上を原点として 0–1000 に正規化した領域の座標が付きます。Web の出典画面では該当領域を枠で表示し、原本をダウンロードできます。

## MCP で PARA と LLM Wiki を作る

```sh
dunlin init ./para-work --preset para
dunlin init ./wiki-work --preset llm-wiki
dunlin serve --stdio --config ./para-work/dunlin.json
```

LLM Wiki を使う場合は `serve` の設定ファイルを `./wiki-work/dunlin.json` にします。MCP クライアントには、`dunlin serve --stdio --config <絶対パス>` を起動コマンドとして登録します。Windows では `node` をコマンドにし、CLI ファイルの絶対パスと `serve` 以降の引数を渡せます。

`authoring.para` と `authoring.wiki` は成果物の保存先です。両方を設定すると、同じ接続で両方のツールを使えます。`authoring.sourceRoot` は `source.ingest` が読み取るローカルファイルの範囲で、初期値はプロジェクトのディレクトリです。URL またはファイルの送信で取り込む場合は `npm --prefix tools/import ci` も実行します。

| 操作 | 使い方 |
|---|---|
| `source.ingest` | `location` に HTTP(S) URL またはファイルパスを指定します。相対ファイルパスは `authoring.sourceRoot` が基準です。同じ資料を再登録すると版を照合します。 |
| `source.upload` | MCP クライアントから `name` と `content` を送ります。ファイル全体を渡す場合は `encoding: "base64"` を使います。UTF-8 のテキスト、Markdown、HTML は 1 MiB、設定済みパーサーで変換する Base64 原本は 16 MiB が上限です。 |
| `source.read` | `sourceId`、`objectId`、必要に応じて `version` を渡して本文を読みます。`nextLine` を `startLine` に渡すと続きが読めます。保存時は返された `evidence` を引用に使います。 |
| `para.context` / `para.save` | 分類ごとのフォルダと既存ノートを確認し、`Projects/認証移行/方針` のような ID に、エージェントが作成した本文と引用を HTML で保存します。ノート ID を `para.context` に渡すと本文と版を取得できます。 |
| `wiki.get` / `wiki.affected` / `wiki.apply` | 既存ページと引用先の文書の版や出典 URL の変更を確認し、エージェントが作成した本文と引用を複数ページまとめて HTML で保存します。`wiki.apply` の `summary` は変更ログに記録します。 |

`para.save` と `wiki.apply` には編集用の本文を `markdown` で渡します。`expectedRevision` は新規作成なら `null`、更新なら読み取ったページの `revision` を渡します。引用の URL・版・行範囲は保存前に索引と照合します。

ファイルを共有していないクライアントから `source.upload` を呼ぶ場合の引数例です。添付ファイルはクライアントが全体を読み、Base64 にして渡します。テキストを直接渡す場合は `encoding: "utf8"` も使えます。

```json
{
  "name": "policy.md",
  "content": "IyBQb2xpY3kK",
  "encoding": "base64",
  "originalUrl": "https://example.com/policy"
}
```

`originalUrl` は引用に使う任意の出典で、省略時は `dunlin://` の参照 ID を発行します。返された `uploadId` を同じ `name` と一緒に渡すと、その資料を更新できます。取り込み後は URL・パス指定と同じ `source.read` で本文と根拠を取得できます。

受信した原本は `index` 配下の `.dunlin-uploads-v1` に保管します。`source.upload` の `original.uri` を MCP の `resources/read` に渡すと、原本を Base64 の `blob` として取得できます。`source.read` の `originals` には同じ文書の保存済み原本が並び、再送した後も以前の原本を取得できます。

添付ファイルからノートやページを作るときは、`source.read` が返す `evidence` をそのまま `para.save` や `wiki.apply` の `citations` に渡します。出典欄にファイル名、該当行、引用した版の原本 URI が記録されます。`originalUrl` を指定した場合は、その URL と原本へのリンクを両方表示します。保存後は `para.context` または `wiki.get` が返す `citations[].original.uri` からも原本を取得できます。以前に取り込んだ添付ファイルに原本リンクを付ける場合は、同じ `uploadId` で再送して引用を保存します。

PARA のノートは `para/Projects` などの分類フォルダに `.html` で保存します。Wiki は `wiki.apply` で `wiki/revisions/<版>/pages/` に HTML ページを作り、同じ版に `index.html` と `log.html` を保存します。すべての保存が終わったら `wiki/CURRENT` を切り替えます。`wiki.get` は公開中の版とファイルの場所を返します。

添付ファイルの送信から、保存した PARA ノートと Wiki ページの出典を使った原本の再取得までは、リポジトリ直下で `node --test tools/rag/test/authoring.test.mjs` を実行すると確認できます。

## ブラウザーで PARA と LLM Wiki を閲覧する

保存した `.html` はブラウザーで直接開けます。分類別の一覧、出典の該当行、原本の取得には、`dunlin.json` の `authoring.para` と `authoring.wiki` に設定したコレクションの閲覧サーバーを使います。

```sh
dunlin serve --web --config ./para-work/dunlin.json
```

[http://127.0.0.1:8787/](http://127.0.0.1:8787/) を開きます。PARA は分類別、LLM Wiki は現在の公開版のページ一覧から閲覧できます。各ページの出典から、コーパスに保存した引用時の版の該当行を開けます。原本が保存されていれば、そこからダウンロードできます。MCP で保存した内容はブラウザーの再読み込みで反映されます。

Web はローカル接続の閲覧専用です。ポートを変更するには `--port` を指定します。MCP と Web を同じプロセスで起動する場合は `dunlin serve --stdio --web --config ./para-work/dunlin.json` を使います。既存の MCP 接続と別のプロセスで Web だけを起動することもできます。
