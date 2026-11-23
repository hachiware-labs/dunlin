# Node.js API

## Node.js ライブラリのインストール

CLI と `KnowledgeNestfile.yaml` を使わず、Node.js のアプリケーションから索引を直接扱う場合は、このライブラリ API を使います。以下の例では `KnowledgeNest.open()` で索引を開きます。公開 API は TypeScript と JavaScript の ESM で利用できます。Rust の Tantivy が索引と検索を担い、NAPI-RS v3 で Node.js に接続します。[Tantivy](https://docs.rs/tantivy/0.27.0/tantivy/) · [NAPI-RS](https://napi.rs/docs/introduction/manual-setup)

ビルド済みのバンドル版を、同じ OS と CPU アーキテクチャの Node.js プロジェクトへインストールします。

```sh
npm install /path/to/knowledge-nest-0.5.0.tgz
```

対象は Node.js 22.13 以降の 22 系と Node.js 24 以降です。ビルド対象は Windows x64、Linux x64 glibc、macOS arm64 と x64 です。Linux バイナリの互換性検証は Ubuntu 24.04 を基準にしています。利用側で Rust をコンパイルする必要はありません。

`knowledge-nest` は仮のパッケージ名で、npm への公開は準備中です。配布物には、OS 別に単独でインストールできるバンドル版と、ルートパッケージの `optionalDependencies` から OS 別のバイナリを取得する分割版があります。[NAPI-RS の配布方式](https://napi.rs/docs/deep-dive/release)

## 検索と根拠取得

```ts
import { KnowledgeNest } from 'knowledge-nest';

const kb = await KnowledgeNest.open('./knowledge');
try {
  await kb.addSource({ id: 'docs', kind: 'filesystem', path: './docs' });

  const result = await kb.search('認証方式 OAuth', { limit: 5 });
  for (const { evidence } of result.hits) {
    const { content } = await kb.read(evidence);
    console.log(evidence.source.url, evidence.range, content);
  }
} finally {
  await kb.close();
}
```

- `open` は指定したディレクトリに索引を保存します。登録済みの内容は、再度 `open` して利用できます。
- `addSource` は UTF-8 の Markdown、テキスト、対応するソースコードや設定ファイルを取り込みます。ファイルは最大 1 MiB です。ディレクトリ登録ではドットで始まる子項目、`node_modules`、シンボリックリンクを走査対象から外します。
- 同じ ID で `addSource` を呼ぶと索引を一括で更新します。走査中のエラーやキャンセルでは以前の索引を維持します。`removeSource(id)` はその ID に属する文書を検索対象から外します。同じハンドルの検索には更新がすぐ反映されます。別のハンドルやプロセスからの更新は、そのハンドルを開き直すと反映されます。同時更新は `BUSY` で失敗する場合があります。
- キーワード索引用のチャンクは Markdown 見出しで区切った最大 32 行です。英数字と識別子は大文字小文字を区別せず、日本語の連続文字はフレーズとして扱います。
- `search` は登録した情報源を横断し、候補、合計件数、適用したプラン ID、検索方式、検索エンジン、Rust 内の所要時間を返します。`sourceIds` で対象を絞れます。件数が `limit` に満たない場合は `fewer_matches` になります。
- `read` は現在の索引が参照する版の本文から、指定された 1 始まりの両端を含む行範囲を返します。元ファイルの編集は再登録時に反映されます。再登録によって版が変わると以前の根拠は `STALE_EVIDENCE` になり、削除後は `NOT_FOUND` になります。
- `open`、登録・削除、検索、本文取得には `AbortSignal` を渡せます。検索の `latencyBudgetMs` はワーカー待ち時間を含み、期限超過は `TIMEOUT` で返します。キャンセルは協調的に処理され、Tantivy の検索や開始済みコミットは処理の途中で中断できません。`close` は実行中の操作にキャンセルを通知し、操作の終了とネイティブハンドルの解放を待ちます。

Knowledge-Nest はローカルの信頼済み利用者を想定しています。索引ディレクトリにはコーパスと保存対象の原本が含まれるため、アクセス範囲は OS の権限で管理してください。文書単位の ACL は今後の対象です。

## 取得済み文書の一括登録

URL や S3 などから取得・抽出した本文は、元の URL と一緒に `replaceDocuments` で登録できます。

```ts
const kb = await KnowledgeNest.open('./knowledge');
try {
  await kb.replaceDocuments('web', [{
    objectId: 'guides/oauth',
    url: 'https://example.com/guides/oauth',
    format: 'markdown',
    content: '# 認証方式\nOAuth を使います。\n',
  }]);
  const { hits } = await kb.search('OAuth', { sourceIds: ['web'] });
  for (const { evidence } of hits) {
    const { content } = await kb.read(evidence);
    console.log(evidence.source.url, evidence.range, content);
  }
} finally {
  await kb.close();
}
```

- `objectId` はソース内で一意の安定した ID です。`url` は絶対 URL で、指定した文字列を出典として返します。`format` は `text` または `markdown` です。本文は 1 文書あたり UTF-8 で最大 1 MiB です。
- 同じソース ID に渡した配列で、索引内の文書を一括置換します。配列から外した文書は削除され、空配列でそのソースを空にできます。入力エラーやコミット前に検出されたキャンセルでは、以前の索引を維持します。戻り値の `files` は文書数、`units` はキーワード索引用のチャンク数です。
- 文書の取得と変換は呼び出し元が行います。行範囲は渡した本文に対する 1 始まりの両端を含む範囲です。登録済みの本文または URL が変わると、以前の根拠は `STALE_EVIDENCE` になります。

`getCorpusDocument({ sourceId, objectId }, version)` で、索引から外れた版を含む全文とメタデータを読み出せます。`version` には検索や `getDocument` の `evidence.source.version` を使います。`parsedVersion` はパース結果を、`originalSha256` は保存した原本を識別します。`SourceDocument.corpus` にパーサー情報や `locations` を指定すると、本文と一緒に版管理されます。

`KnowledgeNest.inspectIndex(path)` は既存のキーワード索引のリビジョン、ソース別の文書数、チャンクがある文書数を返します。索引が未作成の場合は `null` を返します。

## 差分同期

`syncSource` は、指定したファイルまたはディレクトリを走査し、ソース ID ごとに索引と比較します。本文、URL、文書形式から変更を判定し、追加・変更した文書のチャンクを登録し、消えた文書のチャンクを削除します。`syncDocuments` では `corpus` に渡したメタデータの変更も判定します。走査時には本文を読み取るため、更新時刻が同じ編集も検出できます。

```ts
const result = await kb.syncSource({
  id: 'policies',
  kind: 'filesystem',
  path: './policies',
});
console.log(result.added, result.updated, result.deleted);
console.log(result.unchanged, result.indexedUnits);
```

外部から取得済みの文書には `syncDocuments(sourceId, documents)` を使います。配列にはそのソースの現在の全資料を渡します。配列から外れた ID は削除として扱い、空配列でソースを空にできます。指定したファイルやディレクトリ自体を開けない場合は同期を中止します。ソース全体を削除する場合は `syncDocuments(sourceId, [])` を使います。

同期の途中で入力の検証や走査が失敗した場合は、変更をコミットしません。

同期は API を呼び出した時に実行します。`addSource` と `replaceDocuments` は一括置換用の API です。

## 検索方式の比較と切替

両方式は同じ Tantivy 索引を使います。0.1 系で作成した索引はそのまま開けます。

| 組み込みプラン ID | `search` の `mode` | 検索方式 |
|---|---|---|
| `keyword-v1` | `keyword` | いずれかの検索語に一致する候補を BM25 順で返す |
| `keyword-coverage-v1` | `coverage` | 全検索語に一致する候補を先に BM25 順で返し、残りを `keyword` の結果で補う |

`mode` の省略または `auto` はアクティブプランを使います。初期プランは `keyword-v1` です。明示モードはその呼び出しに限ってプランを上書きします。組み込みプランの既定件数は 10 件で、評価済みプランでは評価時の `limit` が既定値になります。呼び出しごとに `limit` を上書きできます。結果の `score` は `keyword` では BM25 スコア、`coverage` では順位の逆数です。

評価セットは、質問とその根拠の引用を並べた UTF-8 JSONL ファイルか、同じ形の配列です。CLI の評価データ（`eval-data import`）と同じ形で、引用が1つなら `doc` と `quote` を直接書けます。`doc` は `ソースID:資料の相対パス` です。`prefix`、`suffix`、`page`、`heading` で、同じ文が複数ある場合の位置を絞れます。

```jsonl
{"question":"認証方式は OAuth ですか","doc":"docs:architecture/authentication.md","quote":"認証には OAuth 2.0 を使う。"}
{"question":"監査記録の保存期間","evidence":[{"doc":"docs:operations/audit.md","quote":"監査記録は5年間保存する。","heading":"保存期間"}]}
```

```ts
const previous = await kb.getActivePlan();
const report = await kb.optimize({
  evaluationSet: './eval/questions.jsonl',
  limit: 5,
  repetitions: 5,
  p95LatencyBudgetMs: 20,
});
console.table(report.candidates);

if (report.selectedPlanId) {
  await kb.activatePlan(report.selectedPlanId);
}

// 前の設定に戻す場合
// await kb.activatePlan(previous.id);
```

- `evaluate` は保存せず比較結果を返します。`evaluate` と `optimize` は一つの索引スナップショットを使い、各クエリのウォームアップ後に両方式を交互に測ります。既定値は上位 10 件、各方式・クエリにつき 3 回です。
- 精度は CLI の `eval` と同じ根拠単位の指標です。引用を現在の資料の中で探し（NFKC と空白の正規化）、上位 K 件の結果がその範囲を覆えば回収とみなします。短い引用（既定 40 文字以下、`shortEvidenceChars`）は全体を、長い引用は文字数の 8 割（`minCoverage`）を覆う必要があります。質問ごとの回収率を平均したものが `evidenceRecallAtK` です。資料が見つからない引用や、1か所に定まらない引用は回収できなかったものとして数え、件数を `unresolvedEvidence` に返します。P95 は全計測サンプルの 95 パーセンタイルを最近順位法で求めます。計測範囲は Rust の検索と根拠情報の取得で、Node.js のワーカー待ちと JS への変換は別です。
- `optimize` は P95 が予算内の候補を、`evidenceRecallAtK`、P95 の順で比較します。選んだプランには方式の版、評価セットのハッシュ、索引リビジョン、比較結果を保存します。予算内の候補がなければ `budget_unmet` を返します。以前の版で保存したプランも、そのまま有効にできます。
- `activatePlan` は現在のハンドルと次回以降の `open` に適用されます。すでに開いている別ハンドルへ反映するには開き直します。保存したプランは索引更新後も使えますが、評価値は実測時の索引リビジョンに対応します。

## 複数ソースの探索

複数のファイルやディレクトリを別々のソース ID で登録できます。`explore` はアクティブプランで横断検索し、検索候補を起点に Markdown のリンクを幅優先でたどります。参照形式のリンクや、別ディレクトリへの相対リンクを扱えます。ソース ID を指定するリンクは `knowledge-nest://policies/security/authentication.md` の形式です。

```ts
await kb.addSource({ id: 'notes', kind: 'filesystem', path: './notes' });
await kb.addSource({ id: 'policies', kind: 'filesystem', path: './policies' });
const controller = new AbortController();
const result = await kb.explore('認証方式 OAuth', {
  sourceIds: ['notes', 'policies'],
  maxSteps: 8,
  timeoutMs: 250,
  signal: controller.signal,
});

for (const { evidence, via } of result.evidence) {
  const { content } = await kb.read(evidence);
  console.log(evidence.source.url, via, content);
}
console.log(result.stopReason, result.issues);
```

- 1 回の探索は一つの索引スナップショットを使います。`sourceIds` は最初の検索とリンク先の両方に適用されます。リンクの参照先は索引から取得します。ネットワークや未登録のファイルにはアクセスしません。Markdown のリンク解析には pulldown-cmark を使っています。[pulldown-cmark](https://github.com/pulldown-cmark/pulldown-cmark)
- 根拠は資料ごとに一件です。検索候補は最上位の該当範囲を使い、リンク先は先頭 32 行までを使います。リンク経由の根拠にはリンク元、行番号、リンクの宛先を含めます。同じ資料を再訪しても根拠を重複させません。
- 最初の検索、検索候補のリンク調査、各リンクの追跡はそれぞれ 1 ステップです。既定の上限は 12 ステップと 1,000 ms です。`seedLimit` で探索を始める検索候補の件数を指定でき、既定値はアクティブプランの件数です。資料ごとに未訪問のリンクを最大 16 件たどります。
- 時間切れ、キャンセル、ステップ上限では、取得済みの根拠と停止理由を `partial` として返します。検索候補の一部を取得した後に索引エラーが起きた場合も、それまでの根拠を `search_failed` とともに返します。リンク先が見つからない、空である、読み取れない、またはリンク件数上限に達した場合は `issues` に記録します。時間予算はネイティブワーカーの待ち時間を含みます。停止は協調的で、実行中の索引検索や Markdown 解析がチェックポイントに戻るまで応答が遅れる場合があります。

## Wiki と PARA

Wiki と PARA のページは、KnowledgeNestfile のプロジェクト（`nest init wiki`、`nest init para`）で書きます。資料の取り込み、質問、検査、アーカイブ、内容への指示、画面は CLI のコマンドで行います。ページはモデルが書き、Knowledge-Nest が引用を原本の版と範囲に照らして確かめてから保存します。 [Wiki・PARA の操作](wiki.md)

Node.js からは、CLI と同じ Rust の実装を `KnowledgeNest.wiki(request)` と `KnowledgeNest.wikiServe(request)` で呼べます。`request` には、プロジェクトの場所、検証済みの設定（`about`、`plan`、`pages`）、解決済みのモデル、コマンドと引数を渡します。形は `WikiCommandRequest` 型のとおりで、CLI もこの形で呼びます。`KnowledgeNest.wiki` は `{ status: 'done', output, warnings }` か `{ status: 'failed', error }` を返します。`output` は CLI の `--json` の `data` と同じです。

```ts
// project: WikiCommandRequest のうち、コマンドと引数以外の項目
const result = await KnowledgeNest.wiki({ ...project, command: 'query', args: ['フィルターの交換周期は'], values: {} });
if (result.status === 'done') console.log(result.output);
```

`KnowledgeNest.wikiServe({ base, settings, token })` は、プロジェクトの画面をブラウザ向けに提供します。画面からの指示は作業として記録し、順に実行します。`signal` を止めると、受け付けた作業を終えてから戻ります。

以前の版にあった、直接開いた索引の資料から抜粋でページを作る `kb.wiki`（`build`、`get`、`affected`、`regenerate`）、同期の結果の `wiki`、MCP の道具 `knowledge.wiki_get` はなくしました。

## Node.js API から MCP を提供する

`kb.serveMcp()` は、直接開いた索引を stdio で公開します。既定のツールは `knowledge.search`、`knowledge.read`、`knowledge.explore` です。サーバーは `knowledge-nest` の Rust の部分（MCP の Rust SDK）で動くので、別のパッケージは要りません。KnowledgeNestfile を使う場合は CLI の起動手順を参照してください。[CLI の起動手順](mcp.md)

```js
import { KnowledgeNest } from 'knowledge-nest';

const kb = await KnowledgeNest.open('./knowledge');
const server = await kb.serveMcp({ transport: 'stdio' });
// アプリケーションの終了時に await server.close()、続けて await kb.close()。
```

## Node.js から CLI を実行する

`nest` コマンドの入口は、`KnowledgeNest.runCli(args, options)` を呼ぶだけの起動役です。コマンドの解釈、実行、表示はすべて Rust で行います。`args` には `process.argv.slice(2)` と同じ形の引数を渡します。結果は標準出力と標準エラーに書かれ、戻り値は終了コードです。`options` には、`serve --print-config` が示す `execPath` と `cli`、パッケージのフォルダ（`package`）、ホームディレクトリ（`home`）を渡します。`serving(active)` はサーバーの開始と終了を知らせるので、その間だけ SIGINT と SIGTERM で `signal` を止めます。コマンドと使用法の一覧は `KnowledgeNest.cliCommands()` で読めます。
