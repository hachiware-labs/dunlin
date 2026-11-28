# MCP: let agents search your documents

[日本語](start-mcp.md) · [All introductions](getting-started.en.md)

Let agents such as Claude Desktop and Claude Code search your own documents and answer with sources. Knowledge-Nest needs no model for this.

## When to use it

- You want an agent to look up your internal procedures and design documents while it works
- You don't want to send your documents to an outside model, and want to try everything locally first
- You want the agent to rephrase and search your documents as often as it needs

## How it works

```text
You ─question→ agent ─knowledge.search→ Knowledge-Nest ─→ passages and sources
                  │                                         │
                  └←────────── reads the original with knowledge.read ─┘
                  └─ the agent's own model answers from that evidence
```

Knowledge-Nest returns evidence, not answers. The agent's own model writes the answer.

Search is not done once loaded; you grow it. After you edit the documents, `nest build` replaces only the ones that changed, and a running agent sees the new content without being stopped. Measure how well it finds answers with questions and the quotes that answer them, and make it find more with a user dictionary and a better setup (section 4). Trying never changes the search being served; `nest release` switches only to a result you checked.

## Compared with other ways

- **Rather than letting the agent read the folder itself.** PDF, Word, Excel and scanned documents are searchable by their Japanese words too. Only the passages that answer come back, with document and line, within a set number of characters, so thousands of documents do not fill the agent's context.
- **Rather than a vector-only search server.** Japanese keyword search, strong where you need the exact word (part numbers, clause numbers, technical terms), runs without an embedding model. Add vector search, which handles rephrasing, for a hybrid.
- **Measured, and optimized.** Register questions with the quotes that answer them (an AI can draft the questions too), and measure the share of evidence search finds. Try candidate settings, such as chunk sizes or the number of results, all at once, rank them, and switch serving only to the one you checked. With search you build yourself, you would set up this evaluation and tuning separately. [Measure and optimize search](guide.en.md)

## 1 Create a project and search

Say your documents are in `./manuals`. If you have none at hand, use `examples/manuals` from the repository.

```sh
nest init mcp ./kb --source ./manuals
cd kb
nest build
nest search "ポンプの点検はいつ実施しますか"
```

| Command | What happens |
|---|---|
| `init mcp` | Creates a project and writes the processing settings to `KnowledgeNestfile.yaml` |
| `build` | Reads the documents, splits them into chunks (the unit of search), and builds a keyword index |
| `search` | Searches with a plain sentence and shows results with document, lines, and heading |

If `build` stops on unsupported files, check the list and skip them with `nest build --skip-unsupported`.

## 2 Connect an agent

```sh
nest serve --print-config
```

Paste the `mcpServers` entry it prints into your MCP client's settings.

| Client | Example settings file |
|---|---|
| Claude Desktop | `claude_desktop_config.json` |
| Claude Code | `.mcp.json` in your project |

Restart the client, and the agent starts Knowledge-Nest and uses it. Ask something:

```text
You      When do we inspect the pump again?
Claude   On the first business day of every month. Before inspecting, stop the pump
         and make sure the power is off.
         Sources: maintenance.md line 3, line 5
```

(The exchange is an illustration.)

The agent gets two tools:

| Tool | What it does |
|---|---|
| `knowledge.search` | Searches the documents and returns the list of passages found, and the text of the best ones. Accepts a count (`limit`), a size of the text in characters (`budget_chars`), and the documents to search (`source_ids`) |
| `knowledge.read` | Reads a passage found by search with line numbers, with lines around it (`around`) if asked, or any lines of a document |

A search answers with short text that is easy for an agent to read and light on its context:

```text
3 passages for "pump inspection". Read more of one with knowledge.read and its ref.
1. docs:maintenance.md#L3-5 — Pump maintenance
2. ...

Text of the best passages:

maintenance.md — Pump maintenance
file:///.../maintenance.md:3
Inspect the pump on the first business day of every month.
...
```

- A **ref** such as `docs:maintenance.md#L3-5` is a document and its lines. The agent cites it as the source, and passes it to `knowledge.read` to read on. `docs:maintenance.md#L1-200` reads any lines, and `docs:maintenance.md` the whole document. A read returns up to 20000 characters by default, and gives the ref to read on with when the rest does not fit.
- The tool descriptions say how many documents there are, in which source folders. An agent decides from them when to use the tools.
- For scores, versions and evidence ranges, pass `format: "json"` to `knowledge.search`. It returns the result of `nest search --json`.

## 3 Update the documents

After adding or editing documents, run `nest build` again.

- Only the changed documents are processed.
- A running `serve` uses the new index from the next request. No need to stop the agent.
- To see what would change first, run `nest build --dry-run`.

## 4 Fit search to your documents

Edit `KnowledgeNestfile.yaml` and `build` again. `nest validate` checks the settings before you run.

| You want to | Do this |
|---|---|
| Find specialist terms that get split apart | Add a user dictionary (`user_dictionary` of `index.keyword`). `nest dictionary suggest` proposes entries |
| Stop answers being buried in long chunks | Lower `size` of `chunk.length`. For documents with headings, use `chunk.structure` |
| Handle rephrased questions better | Add an embedding model for hybrid search (`--embedding-model`) |
| Read PDF, Word, Excel, PowerPoint, HTML | The standard `loader.auto` reads them. For scanned PDFs and images, add OCR with `nest add ocr` |
| Keep some files out | List them in `exclude` of `source.filesystem` |

Whether a change helped can be checked by numbers, not by feel. [Improve search by numbers](guide.en.md) · [Evaluation and dictionary](evaluation.md) (Japanese)

## 5 When something goes wrong

| Symptom | What to do |
|---|---|
| The tools don't appear in the client | Check `command` and `args` in the pasted settings and restart the client. Check that `nest` is on `PATH` |
| `BUILD_REQUIRED` | Run `nest build` first |
| Results are not what you expected | Run `nest search "question" --json` to see exactly what the agent receives |
| `UNSUPPORTED_FILES` | Exclude them with `exclude`, or skip them with `build --skip-unsupported` |

Other cases are in [Troubleshooting](troubleshooting.md) (Japanese).

## Where next

- Return answers to your own app: [RAG](start-rag.en.md)
- Also write the knowledge up as pages for people: [Wiki](start-wiki.en.md)
- Settings and serving in detail: [MCP and RAG](mcp.md) (Japanese)
