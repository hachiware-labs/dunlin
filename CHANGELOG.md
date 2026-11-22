# 変更履歴

## Unreleased（0.5.0 開発版）

- プロジェクト名を Dunlin から Knowledge-Nest に変更しました。リポジトリは https://github.com/hachiware-labs/knowledge-nest です。CLI のコマンドは `nest`、npm パッケージは `knowledge-nest`、設定ファイルは `KnowledgeNestfile.yaml`、データのフォルダは `.knowledge-nest/`、環境変数は `KNOWLEDGE_NEST_*`、Node.js API は `KnowledgeNest` と `KnowledgeNestError` になります。以前の名前のファイルやフォルダは読みません。開発版で作ったプロジェクトは、`Dunlinfile.yaml` を `KnowledgeNestfile.yaml` に改名し、`nest build` で作り直してください。
- MCP、RAG、Wiki、PARA を別々に初期化し、`KnowledgeNestfile.yaml` で構成する CLI を追加しました。RAG は `ask` と、HTTP の回答・検索・MCP エンドポイントを提供します。
- MCP・RAG のキーワード検索で Lindera と IPADIC を標準化し、UTF-8 のユーザー辞書を扱えるようにしました。
- `config` でモデル、提供元、OCR の既定値を登録できるようにし、`init` 時の指定でも切り替えられるようにしました。
- 稼働中の検索で更新後の索引を使い回し、差分構築で変更のない Tantivy セグメントを共有するようにしました。文書取り込みは上限付きで並行処理し、厳密ベクトル検索は `f32` で計算します。
- `dictionary suggest` と `dictionary add` を追加しました。調整用の評価から辞書候補と確認用の質問を出し、候補設定で試せます。
- `eval-data generate` が評価ケースのない文書から順に読み、`--count` で指定した件数まで候補を作るようにしました。引用を検証できない候補だけを除き、理由ごとの件数を表示します。`eval-data review` は `--case` を繰り返して複数件を確認できます。
- `eval-data generate` が作る候補に、質問の種類（`lexical`、`paraphrase`、`identifier`）を付けるようにしました。`--kind` で種類を1つに絞れ、結果の `kinds` に種類ごとの件数が出ます。1つの質問が3つまでの引用を持てるようにし、引用は300文字までにしました。長い文書は先頭からではなく、ケースの引用が少ない部分から、文書の中に散らばる順に読みます。モデルへの指示と返させる形が変わったので、`generation.prompt_version` は3です。
- リリース用の npm パッケージを組み立てる `npm run release:prepare` と、手動実行のワークフロー **Release Knowledge-Nest**、MCP Registry 用の `server.json` を追加しました。
- Node.js API から、直接開いた索引の資料を抜粋してページを作る `kb.wiki`、同期の結果の `wiki`、MCP の道具 `knowledge.wiki_get` をなくしました。Wiki と PARA は、CLI と同じ実装を `KnowledgeNest.wiki` と `KnowledgeNest.wikiServe` で呼べます。
- CLI のコマンドを Rust で実行するようにしました。`--json` の出力、エラーの番号、終了コード、書き出す設定ファイルは以前と同じです。HTML の本文の取り出しと Office 文書の変換も Rust で行い、npm の依存から `cheerio`、`yaml`、`zod`、`docling.rs` をなくしました。`nest dictionary add` は候補設定の `user_dictionary` の行だけを書き換え、他の行の書式とコメントをそのまま残します。
- 構築で、テキスト、Markdown、コードの文書の原本ファイルを作らないようにしました。原本はコーパスに保存した本文から復元できます。Markdown 10,000 件の構築が 6.6 秒から 3.7 秒になりました。Wiki と PARA の取り込みは、ページの引用がリンクする原本ファイルをこれまでどおり作ります。チャンクの検索用の文字列（NFKC）も保存せず、読むときに作り直すようにしました。チャンクの保存領域は約1割になり、ビルドの ID は変わりません。
- Wiki と PARA の `ingress` は、引用が資料と一字一句一致しない記述だけを保存せず、残りの記述でページを作るようにしました。保存しなかった記述は `CITATION_REJECTED` の警告で示します。すべての記述が一致しない場合だけ失敗します。
- `nest use` が `KnowledgeNestfile.yaml` から `index.keyword` の `tokenizer` を消していた問題を直しました。設定を保存するときに、`token` を含む名前をすべて秘密として外していたためです。メッセージのない設定の警告は、`undefined` ではなく場所と対処を表示します。
- `init --source` に存在しないフォルダを渡すと、`SOURCE_MISSING` で止まるようにしました。`build` も、資料のフォルダがないときは `ENOENT` の代わりにフォルダと直し方を示します。`use` や `--build` に存在しないビルドを渡したときは、`BUILD_NOT_FOUND` を返します。
- `--json` を付けないときの表示を、JSON から人が読む短い文に変えました。`status`、`build`、`search`、`eval`、`report`、`validate`、`modules`、`eval-data`、`evidence-map`、`tag`、`use` などが対象です。`ask` は回答に出典の番号を付け、出典ごとにファイル、行、見出しを示します。
- `nest --help` にコマンドの説明を加え、`nest --version` と `nest help <command>` を追加しました。打ち間違えたコマンド、オプション、動作、プロジェクトの種類には、`Did you mean ...?` で近い候補を示します。
- `serve --print-config` が、MCP クライアントの設定に貼れる `mcpServers` の項目を表示するようにしました。`--json` の結果は `command` と `args` のままです。MCP の `knowledge.search` は、プロジェクトの検索で `source_ids` と `budget_chars` だけを説明付きで示します。以前の `sourceIds` と `budgetChars` も受け付けます。
- 日本語のキーワード検索で、「する」「いる」「こと」などの軽い語だけが一致する文書を返さないようにしました。「存在しない語句ゼブラ」が「し」だけで一致していました。
- HTTP で本文が UTF-8 でないときは、UTF-8 で送る方法を示すようにしました。JSON の誤りは位置を、`MODEL_MISMATCH` はプロジェクトのモデル名を示します。
- Windows で、索引の更新がまれに「Could not open, update, or read the index」で失敗する問題を直しました。索引を開いたプロセスが、別の更新を見つけるために `meta.json` を 500 ミリ秒ごとに読んでいて、その間は置き換えられなかったためです。この読み込みをやめました。ウイルス対策ソフトなどがファイルを開いている間は、約2秒まで待ってやり直します。Node.js API で同じ索引を開いた別のハンドルやプロセスの更新は、ハンドルを開き直すと反映されます。CLI の `serve` は、これまでどおり新しいビルドを次の要求から使います。
- モデルが記述に `[1]` などの番号を書いても、Wiki のページや `ask` の回答で番号が重ならないようにしました。
- README を CLI 利用者向けにし、Node.js API、検索評価、Wiki・PARA、設定や運用のガイドを整備しました。

### 以前のプロジェクトから移行する

旧設定の `init.type: rag` で `serve` が stdio 専用の検索プロジェクトを引き続き使う場合は、種別を `mcp` にできます。キーワード索引を `trigram` から `lindera` に切り替える場合は、設定を変更して `nest build --from index` で索引を作り直します。採用した検索設定を提供中のプロセスへ反映するときは `serve` を起動し直します。 [MCP の移行](docs/mcp.md#以前の-init-rag-プロジェクトを移行する) · [日本語検索と辞書](docs/mcp.md#日本語検索とユーザー辞書)
