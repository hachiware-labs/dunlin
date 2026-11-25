# Improve search by numbers: change, measure, compare, release

[日本語](guide.md)

"Search seems to miss. Would smaller chunks help?" This guide is about answering that with numbers instead of a hunch.

You cannot tell how good search is by looking. A setting that helps one question may break another. In Knowledge-Nest, once you register pairs of a question and its answer, every change of setting tells you how many of the questions found their answer. And since only what you changed, and what follows it, is rebuilt, trying is fast, so you can compare as many variations as you like.

It is easiest to read after you have search running with the [MCP](start-mcp.en.md) or [RAG](start-rag.en.md) introduction. Section 2 writes a first configuration, and the sections after it show how to improve it.

> The new commands in this guide support text and Markdown (`.md`, `.markdown`, `.txt`), keyword search, and evidence recall for now. Vector search, user dictionaries, PDF and Office loading, `ask`, and more use the form of configuration that `init` writes (see [10 What still uses the earlier flow](#10-what-still-uses-the-earlier-flow)).

## 1 The idea

```text
documents → extract → transform → index → search → eval
            (docs)    (chunks)    (index)  (query)  (result)
```

- The result of each section is kept as a version in the store (`.knowledge-nest/store`). The same input and the same processing never run twice.
- When you change a setting, **only that section and the ones after it** run. Changing `transform` does not read the documents again.
- A result can have a name. Names live on separate shelves by kind: documents, chunks, indexes, evaluation data, and evaluation results.
- Improving search is a loop: measure a baseline, change one thing, measure again, compare, choose.

## 2 Write a first configuration and measure

### Write the configuration and build

The new commands do not use `init`. They run from a configuration file whose sections are written as lists of steps. From the root of the repository, make a new folder:

```sh
mkdir store-demo
cd store-demo
mkdir docs
cp ../examples/manuals/*.md docs/
```

Create `KnowledgeNestfile.yaml`. Each section (`extract`, `transform`, `index`) lists its steps from top to bottom, and the last `tag` names the result.

```yaml
extract:
  - source.filesystem: { path: ./docs }
  - loader.text: {}
  - tag: docs

transform:
  - chunk.length: { unit: characters, size: 800, overlap: 100 }
  - tag: chunks

index:
  - index.keyword: { tokenizer: lindera }
  - tag: main

search:
  - retrieval.keyword: { top_k: 10 }
```

```sh
nest build
nest search "ポンプの点検はいつ実施しますか"
nest list
```

`build` shows which sections ran and what changed among the documents. Run `build` again with nothing changed and it prints `Nothing ran: every result was there already.` and rebuilds nothing. `list` shows the named versions: documents, chunks, indexes, evaluation data, and evaluation results.

### Measure

Write pairs of a question and the quote that answers it (evaluation data) in `cases.json`:

```json
[
  {
    "id": "pump-check",
    "question": "ポンプの点検はいつ",
    "source": "docs",
    "document": "maintenance.md",
    "quote": "ポンプの点検は毎月最初の営業日に行う。",
    "split": "dev"
  }
]
```

| Field | Meaning |
|---|---|
| `source` | The name of the document folder (the `id` of `source.filesystem`; the folder name if you leave it out) |
| `document` | The file's path inside that folder |
| `quote` | The answer, quoted from a place that appears once in the document |
| `split` | `dev` for tuning, `test` for the final check |

Add an evaluation section at the end of the configuration, and measure:

```yaml
eval:
  - eval.evidence: { data: default, split: dev }
```

```sh
nest eval-data import cases.json
nest eval
```

```text
Recall: 1.000 (1 of 1 cases)
```

Recall is the share of questions whose quote was in the search result. Add questions, change a setting, measure again, compare, and choose. The sections that follow show how.

## 3 Prepare evaluation data

Evaluation data is a set of questions, each with the quote that answers it. Write a JSON array and bring it into the store with `eval-data import`.

```json
[
  { "id": "pump-check", "question": "ポンプの点検", "source": "docs", "document": "manuals/pump.md",
    "quote": "ポンプの点検は毎月最初の営業日に行う。", "split": "dev" },
  { "id": "pump-seal", "question": "シールの交換", "source": "docs", "document": "manuals/pump.md",
    "quote": "型番 P-200 のシールは、2年ごとに交換する。", "split": "dev" }
]
```

```sh
nest eval-data import cases.json
nest eval-data import cases.json --tag v2
```

- `source` and `document` say where the quote is. `source` is the `id` of `source.filesystem` (the folder name if omitted); `document` is the path inside that folder.
- Use a `quote` that appears exactly once in that document.
- Write questions the way your users ask them. A question copied from the text is easy to answer and tells you little.
- Tune on `split: dev` cases. Measure `split: test` cases once, before you adopt a change.
- Importing the same content again makes no new version. Adding cases makes a new version, and the name (`default` or your `--tag`) moves to it.

> `eval-data generate`, which asks a model to propose questions and quotes, is a command of the earlier flow ([evaluation.md](evaluation.md), Japanese). You can write its candidates out as JSON and import them here.

## 4 Measure a baseline and read the misses

Add an evaluation section to the configuration:

```yaml
eval:
  - eval.evidence: { data: default, split: dev }
```

```sh
nest eval
nest results
```

`eval` builds what is missing, measures, and prints the recall:

```text
Recall: 0.667 (2 of 3 cases)
```

`nest results` lists the named evaluation results, best recall first. Give it a name to see each case and where its evidence was found:

```sh
nest results trial-400
```

```text
Result version 7 (trial-400): recall 0.667 (2 of 3), index main-400 with data default
Found at  Case         Question
missed    filter       フィルターの交換
1         pump-check   ポンプの点検
2         pump-seal    シールの交換
```

- `Found at` is the rank at which the quote appeared in the result. `missed` means it did not appear; those cases are listed first.
- Reproduce a miss with `nest search "<question>"` and look for the cause.

| How it misses | What to try |
|---|---|
| The answer is buried in a long chunk | Change `size` and `overlap` of `chunk.length`. For documents with headings, use `chunk.structure` |
| "Replace every 2 years" does not say what it is about | Chunk context (section 5) |
| The answer is found but ranked low | Change `top_k` of `retrieval.keyword` |

## 5 Change one thing and compare (by hand)

Copy `KnowledgeNestfile.yaml` and rewrite only the sections you want to change. To keep the baseline's names where they are, give the changed sections new tags and start from the baseline's documents with `from`.

```yaml
# candidate.yaml
extract:
  - from: docs

transform:
  - chunk.length: { unit: characters, size: 400, overlap: 50 }
  - tag: chunks-400

index:
  - index.keyword: { tokenizer: lindera }
  - tag: main-400

search:
  - retrieval.keyword: { top_k: 10 }

eval:
  - eval.evidence: { data: default, split: dev }
  - tag: trial-400
```

```sh
nest eval candidate.yaml
nest results
```

- `from: docs` does not scan the folder. It uses the version of documents named `docs`, so both runs are measured on the same documents.
- The result is kept as `trial-400`. `nest results` lists it next to the baseline's result, so you can compare recall.
- A comparison is meaningful only if the evaluation data and method are the same. Keep `data` and `split` of `eval.evidence` identical.

## 6 Give chunks some context

A chunk cut out of a document loses which document and which section it came from. With `context`, Knowledge-Nest puts that information at the start of the text the index searches.

```yaml
transform:
  - chunk.structure:
      max_chars: 800
      context: { document: true, headings: true, prepend: true }
```

| Argument | Meaning |
|---|---|
| `headings` | Record the headings above the chunk in the chunk's information |
| `document` | Include the document's name in the context (use together with `prepend`) |
| `prepend` | Put one line of context at the start of the text the index searches |

- The text a search returns, and its position, do not change. Only the string the index searches does.
- Only Markdown `#` headings are read. Other formats give an empty list.
- Changing `context` rebuilds chunks and the index. Documents are not read again.

## 7 Try many settings at once (optimize)

Instead of writing each candidate by hand, describe what to vary. Knowledge-Nest makes the candidates and measures them one after another.

```yaml
# tune.yaml
optimize:
  base: KnowledgeNestfile.yaml
  vary:
    - name: chunking
      options:
        as-is: {}
        length:
          transform:
            - chunk.length: { unit: characters, size: [200, 400], overlap: 0 }
        structure:
          transform:
            - chunk.structure: { max_chars: 800 }
    - name: search
      options:
        top10: {}
        top3:
          search:
            - retrieval.keyword: { top_k: 3 }
  eval:
    data: default
    split: dev
  limit:
    candidates: 20
```

```sh
nest optimize tune.yaml
```

Progress is printed one line at a time (an example; numbers and names depend on the instruction):

```text
[1/N] 000 base
      recall 0.667
...
```

When it finishes, you get a table, best recall first:

```text
Trial  Recall  To base  Ran                  Time   What
010    1.000   +0.333   transform+index+eval ...    chunking=length(size=200),search=top10
000    0.667   -        nothing              ...    base
```

- An **axis** is something to vary; its **options** are the choices. One option from each axis makes one trial. The example combines four chunking choices (`as-is`, two sizes of `length`, `structure`) with two search choices, and also runs the base (trial 0).
- An array as an argument value (`size: [200, 400]`) expands into options on the spot.
- An empty option (`{}`) means "as in the base". The base is always measured, so there is always something to compare with.
- An option replaces a whole section. Two axes that rewrite the same section are an error, and options cannot rewrite `eval`, `tag`, or `from`.
- Trials share what they have in common. Trials that use the same chunks build those chunks once.
- `limit.candidates` caps the number of candidates.
- A failed trial is recorded with its reason and the next one runs. After three failures in a row, the rest are not run.
- Run the same instruction again and finished trials are reused; only the rest run.
- An optimization never moves the `default` of any kind.

To look at results again:

```sh
nest results            # evaluation results, and the optimizations that were run
nest results tune       # the table of the optimization called tune
nest results tune/010   # the cases of one trial
```

Without `--name`, an optimization is called after the instruction file (without its extension).

Each trial's configuration file is written to `.knowledge-nest/trials/<name>/<number>.yaml`. To rerun one trial, give that file:

```sh
nest eval .knowledge-nest/trials/tune/010.yaml
```

> For now you can vary `transform`, `index`, and `search`. Options that rewrite `extract`, measures other than recall, and choosing the next values from earlier results are not there yet.

## 8 Release

What is served changes only when you say so. `build`, `eval`, and `optimize` never change it. Even after you add documents and rebuild, the previous index stays served until you check the new one and `release` it. A deleted document or a wrong setting never reaches users before you have looked.

```sh
nest build                  # make; what is served does not change
nest list indexes           # see the indexes made, and the one being served
nest search "..." --index main
nest eval                   # check
nest release main           # serve the index you checked
```

| Operation | What is served |
|---|---|
| `nest build`, `nest eval`, `nest optimize` | Unchanged |
| `nest build --release` | Switches to the index just made. A shortcut if you want to serve every build |
| `nest release <index> [--name <name>]` | Switches to the index you name. The default name is `release` |

- **By name or by number.** `release` takes an index's name (`main`, `tune/010`) or the version number shown by `nest list indexes`. For the index listed as `v7`, run `nest release 7`.
- **Go back to an earlier version.** Release that index again. It says which version it switched from and to, and the record is never erased.
- `nest list indexes` shows which index is served under which name.

### Let agents and apps search what you released

`nest serve` serves the index that the `tag` of the `serve` section names (`release` when it is not written). When `release` moves the name, a running `serve` uses the new index from its next request, without stopping.

```yaml
serve:
  tag: release
  # without a transport, MCP is served on standard input and output; for HTTP write
  # transport: http
  # port: 4318
```

```sh
nest serve --print-config                  # what to paste into an MCP client
nest serve --transport http --port 4318    # serve /v1/search and /mcp over HTTP
```

- With nothing released, `serve` stops and says what to release.
- The HTTP settings (`host`, `port`, `endpoints`, `access`, `limits`) are those of a RAG project. [Choose what to serve and whom to let in](serve.en.md)
- Search finds up to the `top_k` of the `search` section in chunks, and returns them as a context of up to 4000 characters.
- `/v1/responses` and `nest ask`, which answer, do not work with this form yet: its configuration has no place for an answer model yet.

## 9 Choose which changes of the folder to take in

`apply` of `source.filesystem` chooses which changes of the folder a build takes in.

```yaml
extract:
  - source.filesystem: { path: ./docs, apply: [add, update] }
  - loader.text: {}
  - tag: docs
```

- Choose from `add`, `update`, and `delete`. Without `apply`, all three are taken in.
- Changes that were not taken in are shown by `build` as "Not taken in", and they take effect once you let the setting take them.
- With `[add, update]`, for example, a document removed from the folder stays searchable.

## 10 What still uses the earlier flow
These are not in the new flow yet. Use the configuration that `nest init` writes.

| To do this | Earlier command or page |
|---|---|
| Vector search, hybrid search, reranking | Embedding settings with `index.vector` and `fusion.rrf`. [MCP and RAG](mcp.md) · [Modules](modules.md) |
| User dictionary | `dictionary suggest` and `dictionary add`. [Evaluation and dictionary](evaluation.md) |
| Generate and review questions | `eval-data generate` and `eval-data review`. [Evaluation and dictionary](evaluation.md) |
| PDF, Office, HTML, OCR | `loader.auto` and `nest add ocr`. [PDF extraction](pdf-extraction.md) |
| Answer with `ask` and `/v1/responses` | `ask`, and `responses` of `serve`. [MCP and RAG](mcp.md) |
| Tuning with an agent | [Agents and trials](agents.md) |

Which flow a command uses depends on the form of the configuration file it reads. Sections written as lists of steps such as `- source.filesystem: ...` use the new flow; `modules:` with `use:` uses the earlier flow. One project may hold files of both forms. (Most of the linked pages are in Japanese.)

The two forms differ as follows.

The search commands (`mcp` and `rag`) pick their mechanism by the form of the configuration file they read.

| Form of the file | Example | Mechanism | Commands |
|---|---|---|---|
| The `modules:` and `use:` form | What `nest init mcp` and `init rag` write (the MCP and RAG introductions) | The earlier builds | `build`, `search`, `eval`, `eval-data`, `source`, `transform`, `index`, `status`, `release`, `tag`, `serve`, `ask` |
| Sections as lists of steps (`- source.filesystem: ...` under `extract:`) | The file in section 2 | The versioned store (`.knowledge-nest/store`) | `build`, `search`, `eval`, `eval-data import`, `list`, `results`, `optimize`, `release`, `serve` |

| Feature | The form `init` writes | Lists of steps |
|---|---|---|
| Documents it reads | Markdown, text, HTML, PDF, Word, Excel, PowerPoint, OCR | `.md`, `.markdown`, `.txt` |
| Search | Keyword, vector, hybrid, rerank, user dictionary | Keyword |
| Versions and reuse, comparing many candidates | — | Yes |
| Switch what is served with `release`, `serve` (search over MCP and HTTP) | Yes | Yes |
| `ask`, `/v1/responses` (answers) | Yes | Not yet |

- With lists of steps, `--from`, `--until`, `--force`, `--input`, `--dry-run`, `--skip-unsupported`, and `--tag` of `build`, and `--build`, `--split`, and `--tag` of `eval`, are not available yet.
- One folder may hold configuration files of both forms.
- Wiki and PARA are a separate mechanism from both forms.

## 11 Checklist

- [ ] At least 10 evaluation cases, phrased like real questions
- [ ] The baseline is measured with `nest eval` and its result has a name
- [ ] Trials use the same evaluation method and data as the baseline
- [ ] You looked at how the `missed` cases changed, not only the average
- [ ] You measured with `split: test` before adopting a change
- [ ] You checked the released index with `nest list indexes`
