# Evaluation data and results (staged projects)

An evaluation asks each question of the evaluation data and checks whether the search
returned the chunk that holds the evidence. **Recall** is the share of cases whose evidence
was returned within `top_k`.

## Evaluation data

A JSON file with a list of cases:

```json
[
  {
    "id": "pump-check",
    "question": "ポンプの点検はいつ行いますか",
    "source": "docs",
    "document": "manuals/pump.md",
    "quote": "ポンプの点検は毎月最初の営業日に行う。",
    "split": "dev"
  }
]
```

| Field | What it is |
| --- | --- |
| `id` | A name for the case, unique in the file |
| `question` | What a user would type |
| `source` | The source's name: `id` of `source.filesystem`, or the folder's name (`./docs` gives `docs`) |
| `document` | The file's path below the source folder, with `/` |
| `quote` | Text copied from the document, exactly. A case counts as found when a returned chunk of that document contains this text |
| `split` | `dev` for tuning, `test` for the final check |

Save it:

```sh
nest eval-data import cases.json                # named default
nest eval-data import cases.json --tag v2       # named v2, and default
```

Saving the same cases again makes no new version. Saving changed cases makes one, and
earlier results stay tied to the data they were measured with.

### Writing good cases

The data decides what "better" means, so it deserves care.

- **Copy the quote, do not retype it.** One different character and the case can never be
  found. Read the document and paste.
- **Keep the quote short**: one sentence that holds the answer. A quote that spans two
  paragraphs fails as soon as a chunk boundary falls between them, which measures the
  boundary and not the search.
- **Write the question as a user would**, not as the document words it. If every question
  repeats the quote's words, keyword search scores perfectly and the data cannot tell
  settings apart. Mix: some questions with the document's own words, some paraphrased,
  some asking by a model number or a name.
- **Cover the documents**, not only the easy ones.
- **Aim for a few dozen `dev` cases** before trusting differences. With 5 cases one case
  is 20 points of recall; a difference of one case is noise.
- **Set aside `test` cases** (about one in five) and do not look at them while tuning.

When you write cases for the user, show them the list and say they are generated. A person
who knows the documents should check them before decisions rest on them.

## Measure

The configuration names the data in its `eval` section:

```yaml
eval:
  - eval.evidence: { data: default, split: dev }
```

```sh
nest eval
```

```text
Ran eval.
Recall: 0.800 (4 of 5 cases)
```

`eval` builds what is missing first, so it is safe to run on a fresh project. If the
evaluation data it names does not exist, it stops before building anything.

**When the configuration has no `eval` section**, add one to the project's configuration
file and tell the user you did. The section changes nothing about what is built, and
without it `nest eval` has nothing to measure with. Do not keep a private copy of the
configuration to measure with: the copy drifts from the real one.

**To measure with `test`**, use a second file that differs only in the `eval` section, and
give the result a name of its own:

```yaml
eval:
  - eval.evidence: { data: default, split: test }
  - tag: test
```

Without the tag, the `test` result takes the name `default` away from the `dev` result.
Do this once, on the setting already chosen.

## Compare with before

Each `nest eval` that measures something new makes a new result, and `default` moves to
it. The earlier result stays and is listed without a name:

```text
Results  default  v11  2026-11-22 06:16Z  recall 1.000 (5 of 5), index main, default with data default
         -        v6   2026-11-22 06:14Z  recall 0.800 (4 of 5), index version 5 with data default
```

`nest results 6` shows the cases of the earlier one by its version number. Comparing the
two case by case is how you answer "did it get worse": the same recall can hide one case
gained and another lost.

## Read the results

```sh
nest results                 # every named result, best first
nest results default         # the cases of the latest result
nest results tune/002        # the cases of one trial of an optimization
```

```text
Result version 12 (tune/002): recall 0.800 (4 of 5), index main, default with data default
Found at  Case         Question
missed    pump-seal    シールの交換
1         faq-update   文書を更新したら
2         filter       フィルターの交換
```

`Found at` is the place of the evidence among the returned chunks; `missed` means it was
not within `top_k`. Missed cases come first because they are what to work on.

The heading line also gives the **MRR** (mean reciprocal rank): 1 for evidence returned
first, 0.5 for second, and so on, nothing for a miss, averaged over the cases. Results are
compared by recall, and by MRR when the recall is the same.

With `--json`, each case also has `expected` (source, document, quote) and `returned` (the
chunks that came back, with document and lines). That is what you need to explain a miss.

## Explaining a missed case

For each missed case, find out which of these it is before proposing a change:

1. **The evidence is not in the documents.** The document was deleted or renamed, or the
   quote is not exact. Fix the evaluation data.
2. **The right document came back, the right chunk did not.** The quote sits in a chunk
   that says too little of what the question asks. Try smaller chunks, `chunk.structure`,
   or `context` with `prepend` so headings count.
3. **The right document did not come back at all.** The question's words are not the
   document's words. Keyword search cannot bridge a paraphrase; say so. A larger `top_k`
   only helps if the document is just outside the cut.
4. **It was found, but late** (`Found at` is high). It will be lost when `top_k` shrinks.

`nest search "<the question>" --limit 10` shows what a user would actually get, with the
first line of each chunk.
