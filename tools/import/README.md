# Dunlin 文書取り込みツール

HTTP(S) や S3 から原本を取得してパースし、全文と出典情報を Dunlin の `replaceDocuments` に渡す開発用ツールです。Dunlin は全文をコーパスに保存し、チャンクからキーワード索引を作ります。`--index` には、ライブラリの `Dunlin.open()` で直接開く索引ディレクトリを渡します。Dunlinfile を使って MCP や RAG を作る手順は CLI ガイドを参照してください。[ライブラリ API](../../README.md#nodejs-ライブラリのインストール) · [CLI ガイド](../../docs/mcp.md)

## セットアップ

リポジトリのルートで実行します。

```sh
npm ci
npm run build
npm --prefix tools/import ci
```

## 実行

```sh
node tools/import/cli.mjs --index ./knowledge --source web https://example.com/guide.html
node tools/import/cli.mjs --index ./knowledge --source handbook s3://my-bucket/handbook/
node tools/import/cli.mjs --index ./knowledge --source policy s3://my-bucket/policies/policy.md
node tools/import/cli.mjs --index ./knowledge --source web --keep-original https://example.com/guide.html
```

- 1 回の実行に HTTP(S) URL と S3 URL を複数渡せます。指定したソース ID のキーワード索引は、取得に成功した文書一式で更新します。途中で取得やパースに失敗した場合や、対象文書が 0 件の場合は、以前の索引を保持します。
- HTTP(S) はリダイレクトをたどり、指定した最初の URL を出典に使います。対応形式は UTF-8 のテキスト、Markdown、HTML です。HTML は `main`、`article`、`body` の順に本文を選び、抽出した全文をコーパスに保存します。検索結果の行番号は、その版の本文に対応します。
- S3 では、バケットのみ、または末尾が `/` の URL をプレフィックスとして列挙します。プレフィックスでは `.txt`、`.md`、`.markdown`、`.html`、`.htm` を対象とします。それ以外の URL は単一キーを取得します。キー内の空白や `#` は `%20` や `%23` のようにエンコードします。
- S3 のリージョン・認証情報は AWS SDK の設定から取得します。たとえば `AWS_REGION` と AWS プロファイルを利用できます。単一キーには `s3:GetObject`、プレフィックスには加えて `s3:ListBucket` の権限が必要です。S3 互換の検証環境には `--s3-endpoint` でパス形式のエンドポイントを指定できます。 [AWS SDK の認証情報](https://docs.aws.amazon.com/sdk-for-javascript/v3/developer-guide/setting-credentials-node.html)
- 上限は登録本文 1 文書あたり 1 MiB、抽出前の HTML 4 MiB、1 回の本文合計 64 MiB です。
- パース後の全文と行範囲は、`--index` 配下の `.dunlin-corpus-v1/corpus.sqlite` に文書ごと・版ごとに保存します。JSON 保存は `--corpus-backend json` で選べます。`--keep-original` を指定すると、そのソースの原本も保存します。

取り込み後は、`Dunlin.open('./knowledge')` で同じ `--index` ディレクトリを開きます。開いたインスタンスの `search(query, { sourceIds: ['web'] })` が返す `hits[].evidence` から URL と行範囲を確認できます。`read(evidence)` で該当箇所を取得します。

## 検証

```sh
npm --prefix tools/import test
```

テストはローカルの HTTP と S3 API の模擬エンドポイントを使い、CLI での取得から永続化した索引の検索・該当行の取得まで確認します。
