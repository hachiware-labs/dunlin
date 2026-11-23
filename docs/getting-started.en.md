# Getting started: turn your documents into knowledge AI can use

[日本語](getting-started.md)

This is the entrance for first-time users. Pick what you want to build and go to its introduction. Each one starts from a single folder of documents.

## 1 Where to start

| You want to build | When | Model | Introduction |
|---|---|---|---|
| **MCP**: search for agents | You want Claude or another agent to look things up in your documents and answer | Not needed | [MCP](start-mcp.en.md) |
| **RAG**: an API that answers with sources | You want your app or chatbot to receive answers | Needed to answer | [RAG](start-rag.en.md) |
| **Wiki**: pages of knowledge written by AI | You want your documents written up as pages that people and agents can read | Needed to write | [Wiki](start-wiki.en.md) |
| **PARA**: notes organized by AI | You want piled-up documents and notes organized by how you work | Needed to write | [PARA](start-para.en.md) |

- **Not sure? Start with MCP.** No model to prepare; you can try it in minutes.
- **Search type and compiled type.** MCP and RAG search your documents at each question. Wiki and PARA have an AI read the documents first and write them up as pages or notes.
- **Always sourced.** Search results, answers, and sentences on pages all point back to the place in the original.

For more on choosing, see [How the four forms differ](../README.en.md#how-the-four-forms-differ) in the README.

## 2 What you need

- Knowledge-Nest installed. For now it is built from source; see [Install in the README](../README.en.md#install). You are ready when `nest --help` prints.
- A folder of documents. If you have none at hand, use [examples/manuals](../examples/manuals/maintenance.md) from the repository: a short Markdown file on pump maintenance (in Japanese).
- RAG, Wiki, and PARA need a model. The introductions use `qwen3:4b` on a local Ollama.

To try all four forms at once on the sample, there is also the [Quickstart](quickstart.md) (Japanese).

## 3 Improve search by numbers (new commands)

These are new commands, still in development, for measuring and improving MCP and RAG search by numbers. They keep what Knowledge-Nest makes as versions and measure, as a number, how well search finds the answers. For now they support text and Markdown with keyword search, and `serve` does not yet serve their indexes.

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

Recall is the share of questions whose quote was in the search result. Add questions, change a setting, measure again, compare, and choose. That continues in the [Guide](guide.en.md).

## 4 Where next

- Improve search by numbers: [Guide](guide.en.md)
- All commands: [CLI reference](cli.md) (Japanese)
- When something goes wrong: [Troubleshooting](troubleshooting.md) (Japanese)

## Appendix: the two forms of configuration

The search commands (`mcp` and `rag`) pick their mechanism by the form of the configuration file they read.

| Form of the file | Example | Mechanism | Commands |
|---|---|---|---|
| The `modules:` and `use:` form | What `nest init mcp` and `init rag` write (the MCP and RAG introductions) | The earlier builds | `build`, `search`, `eval`, `eval-data`, `source`, `transform`, `index`, `status`, `use`, `tag`, `serve`, `ask` |
| Sections as lists of steps (`- source.filesystem: ...` under `extract:`) | The file in section 3 | The versioned store (`.knowledge-nest/store`) | `build`, `search`, `eval`, `eval-data import`, `list`, `results`, `optimize`, `release` |

| Feature | The form `init` writes | Lists of steps |
|---|---|---|
| Documents it reads | Markdown, text, HTML, PDF, Word, Excel, PowerPoint, OCR | `.md`, `.markdown`, `.txt` |
| Search | Keyword, vector, hybrid, rerank, user dictionary | Keyword |
| Versions and reuse, comparing many candidates | — | Yes |
| `serve` (MCP and HTTP), `ask` | Yes | Not yet |

- With lists of steps, `--from`, `--until`, `--force`, `--input`, `--dry-run`, `--skip-unsupported`, and `--tag` of `build`, and `--build`, `--split`, and `--tag` of `eval`, are not available yet.
- One folder may hold configuration files of both forms.
- Wiki and PARA are a separate mechanism from both forms.
