# Getting started: turn your documents into knowledge AI can use

[日本語](getting-started.md)

This page is a guided tour for first-time users. First, you let an agent such as Claude search your documents; no model is needed for that. Then you move on to an API that answers, a Wiki that an AI writes, and the new commands that improve search by numbers.

If you already know what you want to build, start at its section. If you are unsure, read [Which document database to build, and when](../README.en.md#which-document-database-to-build-and-when) in the README first.

| You want to build | Section |
|---|---|
| Search for agents (MCP) | [2](#2-let-an-agent-search-your-documents)–[4](#4-what-was-made) |
| An API that answers with sources (RAG) | [5](#5-return-answers-to-your-app-rag) |
| A Wiki written by AI, or PARA notes | [6](#6-let-an-ai-write-a-wiki-wiki-and-para) |
| Better search, by numbers | [7](#7-next-keep-versions-and-measure-by-numbers-new-commands) |

## 1 What you need

- Knowledge-Nest installed. For now it is built from source; see [Install in the README](../README.en.md#install). You are ready when `knowledge-nest --help` prints.
- A folder of documents. This page uses [examples/manuals](../examples/manuals/maintenance.md), which ships with the repository: a short Markdown file on pump maintenance (in Japanese).

Start the commands from the root of the repository.

## 2 Let an agent search your documents

```sh
knowledge-nest init mcp ./kb --source ./examples/manuals
cd kb
knowledge-nest build
knowledge-nest search "ポンプの点検はいつ実施しますか"
```

("When is the pump inspected?") It worked if the result shows "ポンプの点検は毎月最初の営業日に行う。" (the pump is inspected on the first business day of every month) with its source.

| Command | What happens |
|---|---|
| `init mcp ./kb --source ...` | Makes a project in `./kb`. The settings are written to `KnowledgeNestfile.yaml` |
| `build` | Reads the documents, splits them into units of search (chunks), and builds a keyword index |
| `search` | Searches with a plain sentence and shows results with sources |

Now connect an agent:

```sh
knowledge-nest serve --print-config
```

Paste the `mcpServers` entry it prints into your MCP client's settings (`claude_desktop_config.json` for Claude Desktop, `.mcp.json` for Claude Code, and so on). Restart the client and ask:

```text
You      When do we inspect the pump again?
Claude   On the first business day of every month. Before inspecting, stop the pump
         and make sure the power is off.
         Sources: maintenance.md line 3, line 5
```

(The exchange is an illustration; the wording depends on the agent.)

The agent finds passages with Knowledge-Nest's `knowledge.search` and reads the original with `knowledge.read`. The agent's own model writes the answer, so Knowledge-Nest needs no model.

If `build` stops because some files are not supported, check the list. If they can be skipped, run `knowledge-nest build --skip-unsupported`.

## 3 Update the documents

After adding or editing documents, run `knowledge-nest build` again.

- Only the changed documents are processed.
- A running `serve` uses the new index from the next question. There is no need to stop it.
- To see what would change first, run `knowledge-nest build --dry-run`.

## 4 What was made

`KnowledgeNestfile.yaml` in `kb` describes what Knowledge-Nest does and how.

```text
document folder → extracted text → chunks → index → search
   (source)          (source)     (transform) (index) (search)
```

To change the chunk size, read PDFs, and so on, edit this file. `knowledge-nest validate` checks it before you run, and `knowledge-nest modules list` shows the parts you can use. [KnowledgeNestfile defaults](config.md) (Japanese)

## 5 Return answers to your app (RAG)

With MCP, the agent wrote the answer. With RAG, Knowledge-Nest calls a model and writes the answer, with sources, itself. The difference is explained in the [README](../README.en.md#how-mcp-and-rag-differ).

Here a running Ollama with `qwen3:4b` is used. Start from the root of the repository.

```sh
knowledge-nest init rag ./rag --source ./examples/manuals --llm-provider ollama --llm-model qwen3:4b
cd rag
knowledge-nest build
knowledge-nest ask "ポンプの点検はいつ実施しますか"
knowledge-nest serve
```

`ask` numbers the sources in its answer and shows the file and lines of each. `serve` offers answers (`/v1/responses`), search (`/v1/search`), and MCP (`/mcp`) on `127.0.0.1:4318`. With the OpenAI SDK, set `baseURL` to `http://127.0.0.1:4318/v1` and call it. [MCP and RAG](mcp.md) (Japanese)

## 6 Let an AI write a Wiki (Wiki and PARA)

In a Wiki, an AI reads your documents and writes pages with citations. PARA works the same way and sorts notes into Projects, Areas, Resources, and Archives. The model is the same as for RAG. If `serve` is running, stop it, and start from the root of the repository.

```sh
knowledge-nest init wiki ./wiki --purpose "Summarize pump maintenance procedures" --llm-provider ollama --llm-model qwen3:4b
cd wiki
knowledge-nest ingress ../examples/manuals
knowledge-nest serve
```

Open `http://127.0.0.1:4319` in a browser: documents and pages on the left, text and sources in the middle, questions and actions on the right. For PARA, read `init para ./para` instead.

- Every sentence on a page carries a citation to the source. A sentence whose citation does not match the source word for word is not saved.
- Originals are never rewritten. Every change is recorded as a job, and `knowledge-nest undo <job-id>` takes it back.
- `knowledge-nest verify` checks citations and links; `knowledge-nest verify --content` checks for contradictions between pages.
- Ask with `knowledge-nest query`, change a page with `knowledge-nest instruct`, check a citation with `knowledge-nest cite`.

[Wiki and PARA](wiki.md) (Japanese)

## 7 Next: keep versions and measure by numbers (new commands)

From here on are the new commands, still in development. They keep what Knowledge-Nest makes as versions and measure, as a number, how well search finds the answers. For now they support text and Markdown with keyword search, and `serve` does not yet serve their indexes.

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
knowledge-nest build
knowledge-nest search "ポンプの点検はいつ実施しますか"
knowledge-nest list
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
knowledge-nest eval-data import cases.json
knowledge-nest eval
```

```text
Recall: 1.000 (1 of 1 cases)
```

Recall is the share of questions whose quote was in the search result. Add questions, change a setting, measure again, compare, and choose. That continues in the [Guide](guide.en.md).

## 8 When something goes wrong

Add `--json` to a failed command to see `code` and `message` under `errors`.

| Symptom | What to do |
|---|---|
| `knowledge-nest` is not found | Run `npm link` and add the folder from `npm prefix -g` to `PATH` |
| `PROJECT_REQUIRED` | Run in the folder with `KnowledgeNestfile.yaml`, or add `--project <directory>` |
| `SOURCE_MISSING` | Check that `--source`, or `path` of `source.filesystem`, points at a folder that exists |
| `MODEL_REQUIRED`, or the model cannot be reached | Check that Ollama is running and the model has been pulled |
| `... is not on the new store yet.` | That module is not in the new commands yet. Use it in the form `init` writes ([appendix](#appendix-the-two-forms-of-configuration)) |
| `No loader takes N files` | The new commands read only `.md`, `.markdown`, and `.txt` |

Other cases are in [Troubleshooting](troubleshooting.md) (Japanese).

## 9 Where next

- Improve search by numbers: [Guide](guide.en.md)
- Try all four types with the sample: [Quickstart](quickstart.md) (Japanese)
- All commands: [CLI reference](cli.md) (Japanese)

## Appendix: the two forms of configuration

The search commands (`mcp` and `rag`) pick their mechanism by the form of the configuration file they read.

| Form of the file | Example | Mechanism | Commands |
|---|---|---|---|
| The `modules:` and `use:` form | What `knowledge-nest init mcp` and `init rag` write (sections 2–5) | The earlier builds | `build`, `search`, `eval`, `eval-data`, `source`, `transform`, `index`, `status`, `use`, `tag`, `serve`, `ask` |
| Sections as lists of steps (`- source.filesystem: ...` under `extract:`) | The file in section 7 | The versioned store (`.knowledge-nest/store`) | `build`, `search`, `eval`, `eval-data import`, `list`, `results`, `optimize`, `release` |

| Feature | The form `init` writes | Lists of steps |
|---|---|---|
| Documents it reads | Markdown, text, HTML, PDF, Word, Excel, PowerPoint, OCR | `.md`, `.markdown`, `.txt` |
| Search | Keyword, vector, hybrid, rerank, user dictionary | Keyword |
| Versions and reuse, comparing many candidates | — | Yes |
| `serve` (MCP and HTTP), `ask` | Yes | Not yet |

- With lists of steps, `--from`, `--until`, `--force`, `--input`, `--dry-run`, `--skip-unsupported`, and `--tag` of `build`, and `--build`, `--split`, and `--tag` of `eval`, are not available yet.
- One folder may hold configuration files of both forms.
- Wiki and PARA are a separate mechanism from both forms.
