# LLM Wiki を作る

コーパスに保存した本文を LLM が読み、トピック別のページを書きます。Dunlin は引用を検証し、HTML のページ・目次・更新履歴を一つの公開版にまとめます。取り込み元の文書を取り込み直したときは、影響を受けるページを特定して改訂できます。

## 1. Wiki を初期化する

[CLI のセットアップ](../README.md#cli-のセットアップ)を済ませ、リポジトリのルートで実行します。

```sh
dunlin init ./wiki-work --preset llm-wiki
```

`init` が作成した `wiki-work/docs/` に、動作確認用の `policy.md` を UTF-8 で作成します。

```markdown
# バックアップ方針

バックアップの保管期間は30日。設定を変更したら復旧テストを実施する。
```

[MCP クライアントの接続設定](mcp.md#2-mcp-クライアントに登録する)で、設定ファイルに `wiki-work/dunlin.json` の絶対パスを指定します。

## 2. 初版を公開する

MCP クライアントに次のように依頼します。

> `wiki.get` で既存ページを確認してください。`source.ingest` で `docs/policy.md` を取り込み、`source.read` で本文を読んでください。`backup/retention` に保管期間、`backup/restore-check` に変更後の復旧テストをまとめ、両方を引用付きで一度に公開してください。変更概要は「バックアップ Wiki の初版を作成」としてください。

`wiki.apply` は `summary` と `pages` を受け取ります。各ページには `id`、`title`、`markdown`、`citations`、`expectedRevision` を指定します。`citations` は `source.read` が返した該当行の `evidence` の配列です。新規ページの `expectedRevision` は `null` にします。公開時にまとめて渡せるページは最大32件です。

`wiki.get` を引数なしで呼ぶと、公開版の `pages`、`indexPath`、`logPath` を確認できます。ID を渡すと、個別ページの本文・引用・`page.revision` を取得できます。

## 3. 取り込み元の変更を反映する

1. `docs/policy.md` の保管期間を30日から45日に変更し、同じ `location` で `source.ingest` を呼びます。
2. `wiki.affected` で文書の版や出典 URL の変更を検出し、影響を受けるページを一覧します。この例では、同じ文書を引用した二つのページが対象です。
3. 各ページを `wiki.get` で読み、`source.read` でコーパスに保存した現在の版の該当行を取得します。必要に応じてページの本文を直し、記述が同じページも引用を更新します。
4. 各ページの `expectedRevision` に `wiki.get` の `page.revision` を設定し、`wiki.apply` でまとめて公開します。再度 `wiki.affected` を呼び、対象が解消したことを確認します。

改訂を公開するまで、公開中のページの出典は、コーパスに保存した引用時の版の本文を表示します。版の競合や古い引用で保存が拒否された場合は、公開中のページと現在の版の本文を読み直します。[コーパスと原本の設定](../tools/rag/README.md#文書コーパスと原本の設定)

## 4. 公開した版を閲覧する

`wiki.get` が返す `indexPath` の `index.html` は、ブラウザーで直接開けます。そこから `pages/` の各 HTML ページと、`log.html` の更新履歴をたどれます。`wiki/CURRENT` は公開中の版を指します。

出典の該当行を開いたり原本を取得したりするには、閲覧サーバーを起動し、ブラウザーで `http://127.0.0.1:8787/` を開きます。

```sh
dunlin serve --web --config ./wiki-work/dunlin.json
```

以前の Markdown 版も `wiki.get` で読めます。次の `wiki.apply` で、既存ページと更新履歴を引き継いだ HTML 版が作られます。

Node.js から文書本文の抜粋を自動で組み立てる場合は、[ライブラリの根拠付き Wiki API](../README.md#根拠付き-wiki)を使います。
