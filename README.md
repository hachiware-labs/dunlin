# Dunlin

Dunlin は、手元の資料から立ち上げられるローカルのナレッジ基盤です。コマンドと設定の仕様は [Dunlin 設計仕様 統合版 (2)](docs/dunlin-spec.md) を参照してください。

[受け入れ確認](docs/acceptance.md) に、§22 と §27 の確認結果を記録しています。

## CLI の初期化

| 初期化 | 主なコマンド | `serve` の既定 |
|---|---|---|
| `init mcp` | `build`、`search` で出典付きの文脈を取得 | stdio MCP |
| `init rag` | `build`、`search`、`ask` で出典付きの回答を生成 | `127.0.0.1:4318` で `/v1/responses`、`/v1/search`、`/mcp` |
| `init wiki` | `ingress`、`query`、`verify` で資料をページに反映・検査 | `127.0.0.1:4319` で Wiki を表示 |
| `init para` | Wiki と同じ操作で Projects・Areas・Resources・Archives のノートを管理 | `127.0.0.1:4319` で PARA を表示 |

初期化すると、作業ディレクトリに `Dunlinfile.yaml` と `.dunlin/state.sqlite` を作ります。コマンドはそのディレクトリ内で実行するか、`--project <directory>` で対象を指定します。 [4種別の初期設定](docs/dunlin-spec.md#41-初期化と作業ディレクトリ)

MCP と RAG では、`--source <directory>` を省くとプロジェクト内の `docs/` を使います。標準のキーワード検索は同梱の Lindera と IPADIC で分かち書きし、自然文で問い合わせできます。`search --json` の `data`、MCP の `structuredContent`、HTTP の `/v1/search` は共通形式です。RAG の回答を使うには `models.answerer.model` にモデル名を設定します。[起動手順](docs/mcp.md) · [日本語検索とユーザー辞書](docs/mcp.md#日本語検索とユーザー辞書)

よく使うモデルは、たとえば `dunlin config set llm --provider ollama --model qwen3:4b` で一度登録できます。次の `init rag`、`init wiki`、`init para` が、新しい `Dunlinfile.yaml` に書き出します。MCP や RAG でハイブリッド検索を使う場合は、同様に `dunlin config set embedding` で埋め込みモデルを登録できます。[モデルの初期化設定](docs/mcp.md#初期化時にモデルを選ぶ)

OCR を既定で有効にするには `dunlin config set ocr` を実行します。次回の `init` から4種別に適用されます。GPU 実行環境とモデルは `dunlin add ocr` で準備します。 [OCR の初期化設定](docs/dunlin-spec.md#初期化時の-ocr-指定)

Wiki/PARA は `--purpose` で目的を指定し、`Dunlinfile.yaml` の `models.writer.model` を設定してから `dunlin ingress <資料パス>` を実行します。生成した HTML は `site/` に保存されます。`ingress` と `verify --content` は作業 ID と進捗を標準エラーに表示し、`dunlin progress <作業ID> --json` でも確認できます。ブラウザでは順番待ちから完了まで同じ作業ページで追えます。内容検査の指摘には比較した二つの引用と出典が付きます。[Wiki/PARA の操作](docs/dunlin-spec.md#131-コマンド)

## CLI のセットアップ

このリポジトリのルートで実行します。ソースからのビルドには Rust 1.94.1 と Node.js 22.20 以降の 22 系、または 24.12 以降の 24 系を使用します。

```sh
npm ci
npm run build
npm link
dunlin --help
```

`npm link` は、このチェックアウトを `dunlin` コマンドとして登録します。コマンドが見つからない場合は、`npm prefix -g` で確認したフォルダを Windows の `PATH` に追加します。macOS・Linux ではその配下の `bin` を追加します。

## Node.js ライブラリのインストール

以下の API 例では、`Dunlin.open()` から索引を直接操作します。公開 API は TypeScript と JavaScript の ESM で利用できます。Rust の Tantivy が索引と検索を担い、NAPI-RS v3 で Node.js に接続します。[Tantivy](https://docs.rs/tantivy/0.27.0/tantivy/) · [NAPI-RS](https://napi.rs/docs/introduction/manual-setup)

ビルド済みのバンドル版を、同じ OS と CPU アーキテクチャの Node.js プロジェクトへインストールします。

```sh
npm install /path/to/dunlin-0.5.0.tgz
```

対象は Node.js 22.13 以降の 22 系と Node.js 24 以降です。ビルド対象は Windows x64、Linux x64 glibc、macOS arm64 と x64 です。Linux バイナリの互換性検証は Ubuntu 24.04 を基準にしています。利用側で Rust をコンパイルする必要はありません。

`dunlin` は仮のパッケージ名です。npm レジストリへの公開とライセンスの決定は未実施です。GitHub Actions は、単独でインストールできる OS 別のバンドル版と、ルートパッケージの `optionalDependencies` からバイナリを解決する分割版を生成します。[NAPI-RS の配布方式](https://napi.rs/docs/deep-dive/release)

push・PR では Windows x64 と Ubuntu x64 のビルド・配布を検証します。macOS の arm64・x64 を検証するときは、GitHub Actions の **Verify Dunlin** で **Run only the macOS ARM and Intel checks** を選んで実行します。GitHub CLI では `gh workflow run ci.yml -f macos=true` で実行できます。

## 検索と根拠取得

```ts
import { Dunlin } from 'dunlin';

const kb = await Dunlin.open('./knowledge');
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
- 同じ ID で `addSource` を呼ぶと索引を一括で更新します。走査中のエラーやキャンセルでは以前の索引を維持します。`removeSource(id)` はその ID に属する文書を検索対象から外します。別ハンドルからの更新は索引の再読み込み後に反映されます。同時更新は `BUSY` で失敗する場合があります。
- キーワード索引用のチャンクは Markdown 見出しで区切った最大 32 行です。英数字と識別子は大文字小文字を区別せず、日本語の連続文字はフレーズとして扱います。
- `search` は登録した情報源を横断し、候補、合計件数、適用したプラン ID、検索方式、検索エンジン、Rust 内の所要時間を返します。`sourceIds` で対象を絞れます。件数が `limit` に満たない場合は `fewer_matches` になります。
- `read` は現在の索引が参照する版の本文から、指定された 1 始まりの両端を含む行範囲を返します。元ファイルの編集は再登録時に反映されます。再登録によって版が変わると以前の根拠は `STALE_EVIDENCE` になり、削除後は `NOT_FOUND` になります。
- `open`、登録・削除、検索、本文取得には `AbortSignal` を渡せます。検索の `latencyBudgetMs` はワーカー待ち時間を含み、期限超過は `TIMEOUT` で返します。キャンセルは協調的に処理され、Tantivy の検索や開始済みコミットは処理の途中で中断できません。`close` は実行中の操作にキャンセルを通知し、操作の終了とネイティブハンドルの解放を待ちます。

Dunlin はローカルの信頼済み利用者を想定しています。索引ディレクトリにはコーパスと保存対象の原本が含まれるため、アクセス範囲は OS の権限で管理してください。文書単位の ACL は今後の対象です。

## 取得済み文書の一括登録

URL や S3 などから取得・抽出した本文は、元の URL と一緒に `replaceDocuments` で登録できます。

```ts
const kb = await Dunlin.open('./knowledge');
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

`Dunlin.inspectIndex(path)` は既存のキーワード索引のリビジョン、ソース別の文書数、チャンクがある文書数を返します。索引が未作成の場合は `null` を返します。

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
console.log(result.wiki.regenerated, result.wiki.failures);
```

外部から取得済みの文書には `syncDocuments(sourceId, documents)` を使います。配列にはそのソースの現在の全資料を渡します。配列から外れた ID は削除として扱い、空配列でソースを空にできます。指定したファイルやディレクトリ自体を開けない場合は同期を中止します。ソース全体を削除して Wiki も更新する場合は `syncDocuments(sourceId, [])` を使います。

索引のコミット後、同期したソースの資料の版や URL に変化がある Wiki ページを再生成します。復元された参照も対象です。同期の途中で入力の検証や走査が失敗した場合は、変更をコミットしません。コミット後に Wiki の再生成が失敗した場合は `wiki.status` が `partial` になり、`wiki.failures` に理由が入ります。同じ入力で再試行すると、索引の変更がなくても残りの再生成を試みます。

同期は API を呼び出した時に実行します。`addSource` と `replaceDocuments` は一括置換用の API です。

## 検索方式の比較と切替

両方式は同じ Tantivy 索引を使います。0.1 系で作成した索引はそのまま開けます。

| 組み込みプラン ID | `search` の `mode` | 検索方式 |
|---|---|---|
| `keyword-v1` | `keyword` | いずれかの検索語に一致する候補を BM25 順で返す |
| `keyword-coverage-v1` | `coverage` | 全検索語に一致する候補を先に BM25 順で返し、残りを `keyword` の結果で補う |

`mode` の省略または `auto` はアクティブプランを使います。初期プランは `keyword-v1` です。明示モードはその呼び出しに限ってプランを上書きします。組み込みプランの既定件数は 10 件で、評価済みプランでは評価時の `limit` が既定値になります。呼び出しごとに `limit` を上書きできます。結果の `score` は `keyword` では BM25 スコア、`coverage` では順位の逆数です。

評価セットは、検索クエリと関連資料の ID を並べた UTF-8 JSONL ファイルか、同じ形の配列です。`objectId` は検索結果に含まれる資料の相対パスです。

```jsonl
{"query":"認証方式 OAuth","relevant":[{"sourceId":"docs","objectId":"architecture/authentication.md"}]}
{"query":"監査記録 保存期間","relevant":[{"sourceId":"docs","objectId":"operations/audit.md"}]}
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
- 精度はクエリごとの Recall@K と nDCG@K を平均します。同じ資料の複数箇所が上位 K 件に入った場合は最初の一致にだけ得点を付けます。P95 は全計測サンプルの 95 パーセンタイルを最近順位法で求めます。計測範囲は Rust の検索と根拠情報の取得で、Node.js のワーカー待ちと JS への変換は別です。
- `optimize` は P95 が予算内の候補を、nDCG@K、Recall@K、P95 の順で比較します。選んだプランには方式の版、評価セットのハッシュ、索引リビジョン、比較結果を保存します。予算内の候補がなければ `budget_unmet` を返します。
- `activatePlan` は現在のハンドルと次回以降の `open` に適用されます。すでに開いている別ハンドルへ反映するには開き直します。保存したプランは索引更新後も使えますが、評価値は実測時の索引リビジョンに対応します。

## 複数ソースの探索

複数のファイルやディレクトリを別々のソース ID で登録できます。`explore` はアクティブプランで横断検索し、検索候補を起点に Markdown のリンクを幅優先でたどります。参照形式のリンクや、別ディレクトリへの相対リンクを扱えます。ソース ID を指定するリンクは `dunlin://policies/security/authentication.md` の形式です。

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

## 根拠付き Wiki

`wiki.build` は、現在の索引で指定した 2〜32 件の文書の本文を抜粋し、Markdown ページを作ります。各引用番号は `citations` の番号と対応し、出典 URL・版・行範囲を確認できます。文書ごとに最初の非空行から最大 12 行、4 KiB までを抜粋します。途中で短縮した行は `truncated` で区別できます。

```ts
const page = await kb.wiki.build({
  id: 'architecture/authentication',
  title: '認証方式',
  documents: [
    { sourceId: 'notes', objectId: 'architecture.md' },
    { sourceId: 'policies', objectId: 'security/authentication.md' },
  ],
});
console.log(page.markdown);
const reopened = await kb.wiki.get(page.id);
```

ページの資料一覧、引用の版、生成時刻、リビジョンは永続化されます。ページを取得すると、その時点の索引と保存した依存情報を照合します。更新・削除・復元が見つかったページは `stale` になり、影響を受けた引用と抜粋は閲覧結果から外れます。存在しないページの `get` は `null` を返します。

参照元の変更は索引に反映された時点で検知します。`syncSource` と `syncDocuments` は影響ページを自動で再生成します。一括置換後などに手動で確認する場合は `wiki.affected()` で影響ページと資料ごとの変更理由を取得できます。`sourceId` を指定すると、そのソースの変更に絞れます。

```ts
const affected = await kb.wiki.affected({ sourceId: 'policies' });
for (const { id, changes } of affected) {
  console.log(id, changes);
  await kb.wiki.regenerate(id);
}
```

再生成は保存済みの資料一覧を使い、現在の索引から引用とリビジョンを更新します。削除・空文書で引用が不足する場合は `insufficient_evidence` と `unavailable` を返します。欠けた資料の ID は保持し、索引に復元されたときにも影響を検知します。

## Node.js API から MCP を提供する

`Dunlin.serveMcp()` は、直接開いた索引を stdio で公開します。既定のツールは `knowledge.search`、`knowledge.read`、`knowledge.explore`、`knowledge.wiki_get` です。`@modelcontextprotocol/server` と `zod` は `dunlin` の依存パッケージです。Dunlinfile を使う場合は CLI の起動手順を参照してください。[CLI の起動手順](docs/mcp.md) · [MCP TypeScript SDK v2](https://ts.sdk.modelcontextprotocol.io/v2/)

```js
import { Dunlin } from 'dunlin';

const kb = await Dunlin.open('./knowledge');
const server = await kb.serveMcp({ transport: 'stdio' });
// アプリケーションの終了時に await server.close()、続けて await kb.close()。
```

## 開発と検証

Rust 1.94.1 と Node.js 22.20 以降の 22 系、または Node.js 24.12 以降の 24 系を使用します。

```sh
npm ci
npm run build
npm test
npm --prefix tools/rag ci
npm --prefix tools/rag test
npm run test:package
npm run bench -- 1000 300
```

`test:package` は `dist/packages/` に tarball を生成し、空のプロジェクトへオフラインでインストールして検索と本文取得を試します。分割版のローカル検証では対応するバイナリの tarball も指定します。

ベンチマークは合成データで初回起動、索引作成、検索レイテンシ、Rust 内の処理時間、メモリ、Recall@10 と nDCG@10 を計測します。実データでの品質評価と性能目標の確定は後続フェーズで行います。
