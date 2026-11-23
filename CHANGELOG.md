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
- 評価に順位と段階の指標を足しました。`eval` の `modules` に `metric.evidence_rank` を書くと、`scores` に `mrr@k` と `ndcg@k` が出ます。レポートの `stages` には、検索モジュールごと、検索全体、文脈に渡った候補、文脈のそれぞれが覆った根拠の割合と、見つからなかった根拠の理由（`not_retrieved`、`dropped_by_fusion`、`ranked_low`、`over_budget`、`truncated`、`unresolved`）ごとの数が毎回出ます。`report compare` の各指標には、符号検定の p 値 `sign_test_p` が付きます。`metric.evidence_rank` は評価方法の ID を変えないので、以前のレポートとも比べられます。
- MCP・RAG の検索に、次のモジュールと引数を足しました。どれも省くと、以前と同じ設定 ID です。
  - `rerank.cross_encoder`: リランカーで上位候補を並べ替えます。Cohere・Jina 形式の `/v1/rerank`（llama.cpp、vLLM、Infinity も同じ）と、Text Embeddings Inference の `/rerank` に送れます。
  - 検索モジュールの `weight`: `fusion.rrf` で、その検索の順位を重みを付けて数えます。
  - `index.vector`・`hannoy.index` の `document_prefix` と、`retrieval.vector`・`hannoy.search` の `query_prefix`: 埋め込む文の先頭に `passage: ` や `query: ` のような文字列を付けます。
  - `context.assemble` の `expand_lines`: 当たったチャンクを前後の行まで広げて文脈に入れます。検索結果の範囲は変わりません。
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
- Wiki と PARA の `serve` の画面を作り直しました。左の列は、資料の取り込み、ホーム、作業の履歴、運用の決まりと、ページと原本のファイルツリーです。右の列は、開いたものと、それに対してできることです。ホームは目的、質問の欄、ページ・原本・未反映・未対応の指摘の件数、最近更新したページと作業を示します。ページでは、本文の引用番号から出典へ飛べ、質問、指示による修正、検査、アーカイブをその場で行えます。原本を開くと、取り込んだ本文と、それを引用しているページが出ます。作業の結果には、何をして何が変わったかを示します。暗い配色と狭い画面にも対応します。
- Wiki と PARA の `ingress` に URL を渡せるようにしました。画面では左上の **資料を取り込む** に URL を書きます。HTML、PDF、Markdown、テキスト、Office 文書を取得して原本として保存し、出典は元の URL を指します。Shift_JIS や EUC-JP のページは UTF-8 に直します。同じ URL をもう一度取り込むと、同じ資料の新しい版になります。
- `serve.access.allowed_clients` を追加しました。`192.168.11.0/24` のようなアドレスの範囲を書くと、その範囲とサーバーのマシン自身からの接続だけを受け付け、ほかは `403 CLIENT_FORBIDDEN` で断ります。RAG のサーバーと Wiki・PARA の画面の両方に効きます。ループバックの外で待ち受けるときの認証は必須ですが、`allowed_clients` が家や職場のネットワークの範囲（`192.168.0.0/16` など）だけなら、認証なしでも起動できます。
- `nest config set serve` を追加しました。待ち受けるアドレス、ポート、宛先名、接続元の範囲、認証を、この PC のすべてのプロジェクトに対して設定できます（`~/.knowledge-nest/config.yaml` に保存）。`--project <directory>` を付けると、そのプロジェクトの `KnowledgeNestfile.yaml` だけを書き換えます。プロジェクトで標準の値から変えた項目はプロジェクトの値が優先です。`nest config unset serve` で消し、`nest config list --project <directory>` でプロジェクトの設定を表示します。
- Wiki と PARA の `serve` の画面のデザインを変えました。白地に黒の文字と1色のアクセントで、ライトとダークの両方に対応します。ページは、節が3つ以上あると **目次** が付き、広い画面では本文の右に、狭い画面では最初の段落の下に出ます。引用番号は文末の小さな番号札になり、出典はカードで並びます。
- Wiki と PARA のページのフォルダに、目録の `index.html` と作業の記録の `log.html` を書き出すようにしました。目録は、すべてのページをフォルダごとに1行の要約付きで並べ、語で絞り込めます。どちらも単体で開ける HTML で、取り込みや指示のたびに書き直されます。Karpathy の LLM-Wiki の `index` と `log` にあたります。
- `init wiki` が作るページのフォルダを `site/` から `wiki/` に変えました（`serve.pages: ./wiki`）。PARA は `site/` のままです。既存のプロジェクトは、設定に書かれたフォルダをそのまま使います。
- Wiki と PARA の `serve` の画面のトップを目録にしました。すべてのページがフォルダごとに並びます。いちばん上の欄は、題名と本文からページを探す検索です（AI は使いません）。AI への質問と検査は、その下に移しました。
- Wiki と PARA のページのファイル（HTML）を、`serve` の画面と同じデザインにしました。左上に目録へ戻るリンクが付き、節が3つ以上あると目次が出ます。本文の引用番号は、下の出典へのリンクです。古い形のページのファイルは、次の作業のときに書き直されます。
- ページのファイルの中のリンクを、ファイルからの相対パスにしました。ページ ID で書かれたほかのページへのリンク（`/guides/pump`、`pump`）は `pump.html` の形になり、プロジェクト内の原本へのリンクは `file:` の URL ではなく `../../資料/pump.md` の形になります。プロジェクトのフォルダを移してもリンクが切れません。
- Wiki と PARA の `ingress` が、新しい資料を既存のページに書き足すようにしました。これまでは、AI が既存のページの ID を返すと、そのページを新しい資料の内容だけで置き換えていました。いまは、返ってきた文を既存のページの末尾に足し、前の文は残します。同じ文は重ねて書きません。資料の新しい版を取り込んだときだけ、古い版を引用していた文を外します。AI に渡す既存ページの情報は、本文の冒頭 1200 文字から、ID と題名の一覧に変えました。小さいモデル（`qwen3:4b`）が、ページの本文を見るとそれを写すだけになるためです。
- Wiki と PARA でページを書くとき、モデルに出力の上限を渡さないようにしました。モデルが書ける最大の長さまで書けます。待ち時間の上限は 2 分から 30 分にしました。これまでは RAG の回答と並べ替えの上限（1024 トークン、OpenAI は 2048）をそのまま使っていて、1回の取り込みで複数のページを書くと途中で切れ、`Ollama response was incomplete` や `OpenAI response status: incomplete` になっていました。RAG の `ask` は変わりません。OpenAI の応答が途中で切れたときは、理由と使ったトークン数を示します。
- `ingress` で、AI に渡す資料に名前（ファイル名か URL）を付けました。`init wiki` の `rules.md` は、資料の要約ページの ID をこの名前から作るよう指示します。番号しか渡していなかったため、GPT-6 Luna がどの資料の要約も `sources/1` に書き足していました。
- `ingress` で、引用の末尾にモデルが付けた資料の番号（`: 1`、` [1]` など）を外した引用が原本に1か所だけあれば、その引用として扱うようにしました。`qwen3:4b` がこの形の引用を返し、すべて `CITATION_REJECTED` になることがありました。
- `init wiki` が作る `rules.md` に、ページの種類（`sources/`、`entities/`、`concepts/`、`overview`）、書き方、取り込みの手順を書くようにしました。Karpathy の LLM-Wiki の「schema」にあたります。既存のプロジェクトの `rules.md` と、PARA の `rules.md` は変わりません。
- `log.html` を追記式にしました。作業を古い順に並べ、新しい作業を末尾に足します。1件は `[2026-01-02] ingest | スクラムガイド.md` のように、日付、したこと、対象で始まります。質問は質問文で、検査は指摘の件数で記録します。CLI の `verify`（`--content` なし）も作業として記録するようにしました。`--json` の結果に `run_id` が付きます。
- `verify` に指摘を足しました。`MISSING_LINK` は、ほかのページの題名に触れているのにリンクのないページです。`--content` では、ページに出てくるのにページのない概念を `MISSING_PAGE` として挙げ、次に調べるとよい問いと探すとよい資料を `suggestions` に返します。すべての指摘に、ページと内容を書いた `description` が付きます。
- `nest ingest` を `nest ingress` の、`nest lint` を `nest verify` の別名として使えるようにしました。
- `query` の結果に `question` を足しました。回答の引用でモデルがページの番号を取り違えても、その引用のあるページが1つだけなら、そのページの引用として扱います。`qwen3:4b` が出典の番号（`[3]` など）をページの番号として返し、`INVALID_CITATION` で失敗していました。
- Wiki と PARA のページの本体を、`wiki/`（PARA は `site/`）の HTML ファイルにしました。これまでは `.knowledge-nest/state.sqlite` の表が本体で、HTML はそこから書き出したものでした。各ファイルの末尾の `<script type="application/json" id="knowledge-nest-page">` に、ページの ID、題名、本文、引用が読める JSON で入り、コマンドはここからページを読みます。以前の形のプロジェクトは、次に書き込みのあるコマンドを実行したときに、表のページがファイルに移ります。
- `init wiki` が原本のフォルダ `raw/` を作るようにしました。`raw/` があるプロジェクトでは、`serve` の画面から上げたファイルを `raw/` に、URL から取った資料を `raw/<ホスト名>/<パス>` に保存します。これまでは `.knowledge-nest/artifacts/uploads/` に保存していました。
- `raw/` があるプロジェクトでは、`raw/` の外のファイルやフォルダを `ingress` に渡すと、`raw/` にコピーしてから読むようにしました。ファイルは `raw/<名前>`、フォルダは `raw/<フォルダ名>/…` に置き、元の更新日時を引き継ぎます。同じ名前で中身の違うファイルは上書きせず、名前にハッシュの先頭8文字を付けて別に置きます。資料は元のパスで識別するので、変えたファイルを取り込み直すと同じ資料の新しい版になります。
- Wiki と PARA のページの出典を、文書ごとにまとめて示すようにしました。出典の一覧は文書ごとに1つで、引用した行が並びます（同じ行は1回）。本文の番号は文書の番号になり、同じ文書から続く文には、その続きの最後に1回だけ付きます。節が変わると付け直します。目録と画面の「出典 n 件」も文書の数です。ページの記録は文ごとの引用のままで、`cite` の番号も変わりません。前の形のページのファイルは、次の作業のときに書き直されます。
- Wiki のページを、フォルダ（`concepts/`、`sources/` など）ではなく、何についてのページかという分類で並べるようにしました。ページは `wiki/` に平らに置きます。分類の一覧は `rules.md` の `## 分類` に書き、目録と画面はその順に並びます。取り込みで AI が一覧にない分類を付けたときは、その分類が一覧に足されます。各ページの分類はファイル末尾の JSON の `category` にあり、取り込みでは変えません。`nest instruct <ページ> --category <分類>` で付け替えられ、`undo` で戻せます。`init wiki` の `rules.md` から、`sources/`・`entities/`・`concepts/` の種類分けを外し、資料ごとの要約ページの ID を `要約-<資料の名前>` にしました。PARA は変わりません。
- `init wiki` の `rules.md` から、全体の見取り図のページ（`overview`）と、取り込みのたびにそれを今の内容に合わせる手順を外しました。全体は目録（`wiki/index.html`）で見ます。見取り図は取り込みのたびにほぼ必ず書き直しの対象になり、ページが増えるほど取り込みを重くしていました。
- Wiki と PARA のページの出典を、文ごとの引用から、ページ単位の資料の一覧に変えました。AI はページを Markdown で書き、使った資料の番号を添えます。ページの記録（ファイル末尾の JSON）は `citations` の代わりに `sources` を持ち、本文に ` [1]` などの番号は付きません。ページのファイルと画面の出典は資料の一覧になり、行ごとの「引用箇所を見る」はなくなりました。文ごとに引用していた前の形のページは、番号を外した本文と、引用していた文書の一覧として読み、次に更新されたときに新しい形で書き直します。引用を一字一句写させる手間がなくなり、AI はページを自然な文で書けます。
- 文ごとの引用の代わりに、本文の数字（2桁以上）・日付・「」『』“” の引用が出典の資料にあるかを、プログラムが照らすようにしました。ないものは、取り込みでは `UNSUPPORTED_LITERAL` の警告に、`verify` では `UNSUPPORTED_LITERAL` の指摘に出ます。`verify` の `CITATION_MISSING` は、出典の資料がない `SOURCE_MISSING` に変わりました。`STALE_VERSION` は出典の資料ごとに出ます。
- 取り込みで、どのページにも使われなかった資料を **得るものなし** として記録するようにしました（作業の結果の `no_material`、記録の「得るものなし：…」、目録と画面の `raw/` の表示）。`init wiki` の `rules.md` では、要約ページを、その資料ならではの内容があるときだけ作るものにしました。
- 書き足し（`append`）の取り込みは、AI が返した本文のうち、ページにまだない段落だけを足すようにしました。資料の新しい版を取り込んだときは、その資料を出典に持つページの本文を AI に渡し、書き直させます。書き直し（`rewrite`）で前のページの本文と引用箇所を渡すのをやめ、`STATEMENTS_REMOVED` の警告はなくなりました。
- `nest cite <ページ> <番号>` は、ページの出典の番号の資料と、そのいまの本文（`content`）を返すようにしました。
- PARA を Tiago Forte の PARA に合わせて作り直しました。`init para` の `rules.md` に、人が取り組みを書く `## Projects`、領域を書く `## Areas`、AI が話題を足す `## Resources`、`## Archives`、ノートの書き方と置き場所の決め方が入ります。AI は使い道のある資料1件ごとに、1か所にノート（要点と使い道）を書き、`Projects/<取り組み>/`、`Areas/<領域>/`、`Resources/<話題>/` のうち、それを使う場所に置きます。決まりにない取り組み・領域には置かず（`PLACE_INVALID` の警告。資料は未反映のまま残る）、Resources の新しい話題は `rules.md` に足します。資料の写しは、ノートと同じフォルダの `資料/` に置き、ノートの出典はそこを指します。AI には置ける場所の一覧を渡し、AI は置き場所だけを決めます。ノートの名前は資料の名前から付け、同じ資料を別の場所にも書いたときは最初の1つだけを残します（`NOTE_SKIPPED`）。小さいモデル（`qwen3:4b`）でも置き場所を外さないようにするためです。
- PARA の `nest archive Projects/<取り組み>` で、取り組みのノートと資料をフォルダごと `Archives/<取り組み>` へ移せるようにしました。ほかのノートからのリンクも付け替え、`rules.md` の行を `## Archives` へ移します。`nest archive --restore Archives/<取り組み>` と `nest undo` で戻せます。画面では、ノートの **検査・アーカイブ** にボタンが出ます。`verify` は、決まりにない取り組み・領域のノート（`UNKNOWN_PROJECT`、`UNKNOWN_AREA`）、置き場所のフォルダのないノート（`NO_PLACE`）、期限を過ぎた取り組み（`PROJECT_OVERDUE`）を指摘します。
- Wiki と PARA で URL を取り込むとき、このマシンの Chrome か Edge でページを開き、表示された HTML を原本にするようにしました。スクリプトで本文を描くページも取り込めます。ブラウザは Rust から Chrome DevTools Protocol で動かし、画面を出さず、取り込みのたびに一時プロファイルで起動します。PDF などページでないものは、これまでどおりダウンロードします。`loader.auto` の `fetch`（`browser` か `http`）と `browser`（`auto`、`chrome`、`edge`、パス）で選べ、`nest ingress --fetch http` でその場だけダウンロードにできます。ブラウザが見つからないときは、ダウンロードして `FETCH_WARNING` を出します。使った取り方は作業の結果の `fetched` に残ります。
- `verify` に、分類のないページ（`NO_CATEGORY`）と、`rules.md` にない分類のページ（`UNKNOWN_CATEGORY`）の指摘を足しました。`--content` では、AI が分類の合っていないページと合っている分類を挙げます（`CATEGORY_MISMATCH`）。画面ではこの指摘から分類を付け替えられます。
- Wiki の目録（`wiki/index.html`）と `serve` の画面の左の列を、`wiki/` と `raw/` の2つの見出しにしました。ページは `wiki/` のフォルダ名（`concepts/`、`sources/` など）のとおりに、原本は `raw/` のフォルダとファイルのとおりに並びます。原本にはそれぞれ **反映済み**・**未反映**・**未取り込み** が付きます。これまでは「考え方・用語（concepts）」のように言い換えたフォルダ名と、取り込んだときのファイル名で並べていました。
- `ingress` に、既存のページを書き直す形を足しました。`wiki.write` の `update: rewrite` では、AI がページの一覧から資料が関係するページを選び、その本文と引用箇所を読んで書き直します。外れた文は `STATEMENTS_REMOVED` の警告と作業の結果に出ます。`update: append` は、これまでどおり文を足すだけです。`update` を省くと、Ollama では `append`、OpenAI と Strata では `rewrite` になります。
- README を CLI 利用者向けにし、Node.js API、検索評価、Wiki・PARA、設定や運用のガイドを整備しました。

### 以前のプロジェクトから移行する

旧設定の `init.type: rag` で `serve` が stdio 専用の検索プロジェクトを引き続き使う場合は、種別を `mcp` にできます。キーワード索引を `trigram` から `lindera` に切り替える場合は、設定を変更して `nest build --from index` で索引を作り直します。採用した検索設定を提供中のプロセスへ反映するときは `serve` を起動し直します。 [MCP の移行](docs/mcp.md#以前の-init-rag-プロジェクトを移行する) · [日本語検索と辞書](docs/mcp.md#日本語検索とユーザー辞書)
