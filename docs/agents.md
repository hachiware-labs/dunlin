# エージェントに検索改善を任せる

この手順は MCP・RAG のキーワード検索や検索設定を、外部のコーディングエージェントと調整する場面を想定しています。依頼時に、プロジェクトの場所、基準ビルドのタグまたは ID、評価レポートのタグ、変更してよい区切りを渡します。

## 調整用データで試す

`BASELINE_BUILD` は基準ビルドのタグまたは ID、`baseline` は調整用レポートのタグです。例ではチャンク化の引数を試します。`candidate.yaml` を `KnowledgeNestfile.yaml` と同じ場所に作成し、変更する引数を記録します。

```sh
knowledge-nest status --json
knowledge-nest report show baseline --json
knowledge-nest validate --config candidate.yaml --json
knowledge-nest build --from transform --input BASELINE_BUILD --config candidate.yaml --tag trial --json
knowledge-nest eval --build trial --tag trial-eval --json
knowledge-nest report compare baseline trial-eval --json
```

検索設定だけを試す場合は `search --build BASELINE_BUILD --config candidate.yaml --json` で確認し、同じ設定を `eval --build BASELINE_BUILD --config candidate.yaml --tag trial-eval` に渡します。辞書を変える場合は `dictionary suggest` で候補を調べ、`dictionary add` と `build --from index` を使います。 [辞書の試行例](evaluation.md)

## 結果を読む

- `status` が `failed` の場合は `errors[].code` と `message` を確認します。エラーの調査には `run_id` があると追跡できます。
- `data.comparable` を確認し、`data.scores` と `data.cases` から回収率、改善した質問、悪化した質問を報告します。レポートの検索設定や評価方法も確認します。
- 試行ごとに、変更した引数、ビルドと評価レポートのタグ、根拠の回収と速度への影響を残します。`TRIAL_LIMIT_REACHED` では、設定した上限とどこまで試せたかを報告します。

正解の引用と評価方法は比較の基準として固定します。候補を選んだ後の最終確認用分割の実行と `use` による採用は運用担当者が行います。採用の判断に必要な候補設定と結果を担当者に渡します。 [CLI の JSON と終了コード](cli.md#終了コードと-json) · [試行の上限](mcp.md#試行に上限を設定する)
