# PARA を始める

手元の資料を取り込み、コーパスに保存した本文を引用して HTML ノートを作ります。MCP クライアントの LLM が分類と本文を組み立て、Dunlin が引用を照合してノートを分類フォルダに保存します。

## 1. 作業フォルダを作る

[CLI のセットアップ](../README.md#cli-のセットアップ)を済ませ、リポジトリのルートで実行します。

```sh
dunlin init ./para-work --preset para
```

`para-work/dunlin.json`、資料を置く `para-work/docs/`、`para-work/para/` 以下の四つの分類フォルダが作られます。ノートの ID は分類名から始め、拡張子を省きます。

| 分類 | 置くもの | ノート ID の例 |
|---|---|---|
| `Projects` | 完了条件がある取り組み | `Projects/バックアップ見直し/方針` |
| `Areas` | 継続して責任を持つ業務 | `Areas/インフラ運用/点検` |
| `Resources` | あとで参照する知識や資料 | `Resources/バックアップ/用語` |
| `Archives` | 終了した取り組みや過去の資料 | `Archives/旧バックアップ手順/記録` |

動作確認用に `para-work/docs/policy.md` を UTF-8 で作成します。

```markdown
# バックアップ方針

バックアップの保管期間は30日。見直し後は復旧テストを実施する。
```

## 2. MCP クライアントでノートを作る

[MCP クライアントの接続設定](mcp.md#2-mcp-クライアントに登録する)で、設定ファイルに `para-work/dunlin.json` の絶対パスを指定します。接続後、クライアントに次のように依頼します。

> `para.context` で分類と既存ノートを確認してください。`source.ingest` で `docs/policy.md` を取り込み、`source.read` でコーパスに保存した本文を読んでください。保管期間と復旧テストを整理し、引用付きで `Projects/バックアップ見直し/方針` に保存してください。

クライアントは `source.ingest` が返した `document` の `sourceId`、`objectId`、`version` を `source.read` に渡します。`para.save` には `id`、`title`、`markdown`、`citations`、`expectedRevision` を渡します。`citations` には `source.read` が返した `evidence` を配列で渡し、新規ノートの `expectedRevision` は `null` にします。

既定の `authoring.sourceRoot` は作業フォルダです。別のフォルダにある資料を扱う場合は、そこを `authoring.sourceRoot` に設定して接続し直すか、クライアントから `source.upload` で送ります。添付の原本は自動で保存されます。`source.ingest` でテキストや HTML の原本も保存する場合は `keepOriginal: true` を指定します。[原本の保存設定](../tools/rag/README.md#文書コーパスと原本の設定)

## 3. 閲覧と更新

保存先は `para-work/para/Projects/バックアップ見直し/方針.html` です。ファイルをブラウザーで直接開けます。分類別の一覧や、出典の該当行・原本を開くには、別のターミナルで閲覧サーバーを起動します。

```sh
dunlin serve --web --config ./para-work/dunlin.json
```

ブラウザーで `http://127.0.0.1:8787/` を開き、`Projects` からノートを選びます。出典を開き、保管期間と復旧テストの記述を確認できます。

取り込み元の文書を編集したら、同じパスで `source.ingest` を呼び、現在の版の `evidence` を取り直します。保存済みノートの出典からは、コーパスに保存した引用時の版の本文を読めます。ノートの更新には `para.context` にノート ID を渡し、返された `note.revision` を `para.save` の `expectedRevision` に指定します。競合が返ったら、最新のノートと現在の版の本文を読み直してから保存します。

以前の `.md` ノートも `para.context` で読めます。更新すると同じ ID の `.html` が作られ、以後は HTML が優先されます。
