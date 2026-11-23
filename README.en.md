# Knowledge-Nest

[日本語](README.md)

**Turn the documents you have into knowledge an AI can use and cite. A few commands, and no programming.**

Hand Knowledge-Nest a folder of documents (Markdown, PDF, Word, Excel, web pages ...) and it puts them in a form an AI can use. Your documents and index stay in a folder on your machine, and answers and pages always carry their sources.

## One tool, four kinds of document database

You would usually pick and combine a vector database, a RAG framework, a wiki tool and a notes app. Knowledge-Nest makes all four from the same folder of documents, on its own.

```text
                                         ┌─ MCP    search that agents query
Folder of documents ──→ Knowledge-Nest ──┼─ RAG    answers with sources, returned to your app
(Markdown, PDF, Word, Excel ...)         ├─ Wiki   knowledge pages everyone uses
                                         └─ PARA   notes that organize your own work
```

| What you want | What you make | AI model | How to use it |
|---|---|---|---|
| Let Claude or another agent look things up in my documents | **MCP**: search that agents query | Not needed | [MCP guide](docs/start-mcp.en.md) |
| Return answers based on my documents in a chat app or internal tool | **RAG**: an API that answers with sources | Needed to answer | [RAG guide](docs/start-rag.en.md) |
| Write up my documents as knowledge pages my team uses | **Wiki**: knowledge pages everyone uses | Needed to write | [Wiki guide](docs/start-wiki.en.md) |
| Organize the documents and notes of my own work by project | **PARA**: notes that organize your own work | Needed to write | [PARA guide](docs/start-para.en.md) |

- **One thing to learn.** One install, and one way to start: `nest init <form>`. Loading documents, citing sources, the configuration file (`KnowledgeNestfile.yaml`) and serving with `nest serve` are the same for every form.
- **Grow from the same documents as needs change.** Start with search, then add the answer API or a Wiki from the same folder of documents later, with nothing to rebuild or migrate.
- **Agents can reach every form.** All four are served over MCP, so agents such as Claude use the documents themselves and the knowledge an AI wrote from them in the same way.
- **Stays on your machine.** Documents and indexes are kept in the project folder. The only thing sent out is the text needed for a model you set up yourself.

If unsure, start with MCP: it needs no model. See [Install](#install); a step-by-step walkthrough is in [Getting started](docs/getting-started.en.md), and how the four forms differ is in [How the four forms differ](docs/forms.en.md).

## And each one stands on its own

It does not just make them all. Compare each form with other libraries and tools of the same kind, and there is still a reason to choose Knowledge-Nest. The comparison, including when another way suits better, is in [Where Knowledge-Nest fits](docs/comparison.md) (Japanese).

### MCP: search that agents query

Compared with: filesystem MCP servers (the agent reads files directly) and MCP servers of vector databases (Chroma, Qdrant and others)

| With other MCP servers | With Knowledge-Nest |
|---|---|
| Reading files directly, PDF and Office are hard to handle, and large documents fill the agent's context | PDF, Office and scanned documents become text, and only the passages that answer come back, with document and line |
| Vector search needs an embedding model, and can miss exact part numbers or technical terms | Japanese keyword search runs at once with no embedding model; add vectors for a hybrid |
| Loading, chunking and keeping documents up to date are yours to build | Hand over a folder: it loads, chunks, and updates only the documents that changed |
| Whether search finds the answers is a matter of feel | Measure recall, try candidate settings at once, and optimize |

[MCP guide](docs/start-mcp.en.md)

### RAG: an API that answers with sources

Compared with: RAG frameworks such as LangChain and LlamaIndex, and no-code RAG products where you register documents on a screen

| With other RAG | With Knowledge-Nest |
|---|---|
| Loading, chunking, indexing, search and generation are written in code, and you stand up the API yourself | A configuration file and commands bring up an API the OpenAI SDK calls as it is |
| What happens with no evidence, and how sources are cited, are yours to build | No evidence, no answer; sources point to the version of the document and the range in the original |
| Evaluating answers means combining a separate evaluation library | Drafting questions, measuring, comparing candidates and optimizing are all in the same tool |
| No-code products have you register documents on their server | Documents and index stay in a folder on your machine |

[RAG guide](docs/start-rag.en.md)

### Wiki: knowledge pages everyone uses

Compared with: scripts implementing Karpathy's LLM-Wiki, the AI features of notes apps (Notion, Obsidian and others), and chats that answer questions about documents

| With other tools | With Knowledge-Nest |
|---|---|
| A chat about documents loses its answer when it ends | What was read piles up as pages with sources, and a revised document gets only its own pages rewritten |
| Text a notes app's AI wrote does not show what came from the documents | A program checks at every ingest that numbers, dates and quotations are in the sources, and points out contradictions and terms with no page |
| LLM-Wiki scripts center on ingesting and writing out | Checks, undo by job, a contents page and a log, categories, a browser screen, MCP, and ingesting pages behind a sign-in and X all come with it |
| Field knowledge not in any document blends into the text | It is kept as one of the sources, with who said it |

It is Karpathy's LLM-Wiki idea, with checks against sources added. [Wiki guide](docs/start-wiki.en.md)

### PARA: notes that organize your own work

Compared with: running PARA in Notion or Obsidian, with templates or by hand

| With PARA in a notes app | With Knowledge-Nest |
|---|---|
| Each new document needs you to decide which project it serves | The AI reads it, writes a note, and puts it in the project that will use it |
| Filing ends at putting it in a folder | The note says how this document serves this project |
| Left to an AI, folders tend to multiply | Only the projects and areas you listed in `rules.md` can be used |
| A finished project's notes and documents are put away by hand | `nest archive` moves notes, document copies and links to Archives at once, `nest undo` brings them back, and projects past their deadline are pointed out |

Agents answer from the notes of your current projects over MCP too. [PARA guide](docs/start-para.en.md)

## Rearrange it, and add to it (MCP and RAG)

MCP and RAG are built from modules listed by stage: loading, chunking, index, search and answer.

- **Rearrange.** Adding vector search or a reranker, or chunking along headings, takes a few lines of the configuration. [Modules](docs/modules.md) (Japanese)
- **Formats it cannot read go through your own converter.** Name a conversion command in `loader.command`, and its output is taken in as the text.
- **Models can be swapped.** Embedding and answer models come from a local Ollama or an API such as OpenAI's; a reranker from any server with `/v1/rerank` or `/rerank`.
- **Want a new module? Go ahead.** Define it in the registry in `src/registry.rs`. Knowledge-Nest is MIT licensed, the steps are in [Contributing](CONTRIBUTING.md), and pull requests are welcome.

How to measure and optimize search is in [Improve search by numbers](docs/guide.en.md).

## Install

Publishing to npm is being prepared. For now, build from source. You need Rust 1.95 or later and Node.js 22.20 or later in the 22 line, or 24.12 or later in the 24 line.

```sh
git clone https://github.com/hachiware-labs/knowledge-nest
cd knowledge-nest
npm ci
npm run build
npm link
nest --help
```

## Documentation

Pages without an English version are in Japanese.

| What you want | Pages |
| --- | --- |
| Get started | [Getting started](docs/getting-started.en.md) (from what you want to do to each introduction). To try all four at once, [Start with the sample](docs/quickstart.md) |
| Let Claude look things up, or have an app answer | [MCP](docs/start-mcp.en.md) · [RAG](docs/start-rag.en.md) · [Starting MCP and RAG](docs/mcp.md) |
| Build a knowledge base, or sort documents and notes | [Wiki](docs/start-wiki.en.md) · [PARA](docs/start-para.en.md) · [Using Wiki and PARA](docs/wiki.md) |
| Improve search by numbers | [Improve search by numbers](docs/guide.en.md) · [Evaluation and dictionary](docs/evaluation.md) · [Agents and trials](docs/agents.md) · [Techniques that could raise accuracy](docs/retrieval-techniques.md) |
| Use it from other machines | [Choose what to serve and whom to let in](docs/serve.en.md) |
| Compare the four forms, and other tools | [How the four forms differ](docs/forms.en.md) · [Where Knowledge-Nest fits](docs/comparison.md) · [Benchmarks](docs/benchmarks.md) |
| Look up commands and settings | [CLI](docs/cli.md) · [Modules](docs/modules.md) · [KnowledgeNestfile defaults](docs/config.md) · [Troubleshooting](docs/troubleshooting.md) |
| Embed in Node.js | [Node.js API](docs/api.md) |
| Understand how it works | [Builds and evaluation](docs/concepts.md) · [Data management design](docs/data-management.md) · [Architecture](docs/architecture.md) · [PDF extraction](docs/pdf-extraction.md) · [Design specification](docs/knowledge-nest-spec.md) |
| Develop and operate | [Contributing](CONTRIBUTING.md) · [Changelog](CHANGELOG.md) · [Security](SECURITY.md) · [Acceptance checks](docs/acceptance.md) |
| Licenses and notices | [License](LICENSE) · [Third-party notices](THIRD_PARTY_NOTICES.md) |

## Development

```sh
npm ci
npm run build
npm test
npm --prefix tools/rag ci
npm --prefix tools/rag test
npm run test:package
npm run bench -- 1000 300
```

`test:package` builds tarballs in `dist/packages/`, installs them offline into an empty project, and tries search and reading text. The benchmark measures indexing, search latency, memory, Recall@10, and nDCG@10 on synthetic data. The checks to run before a change, and how to start CI, are in [Contributing](CONTRIBUTING.md).

## Status

- This is a development version, 0.5.0. Commands and configuration may change.
- `knowledge-nest` is a placeholder package name. Publishing to npm is being prepared. The license is MIT.
- Tested environments: Windows x64, Linux x64 (glibc), and macOS arm64 and x64.
- The versioned store and its commands (`list`, `results`, `optimize`, `release`) are being implemented on the `data-management` branch. A command picks its mechanism by the form of the configuration file it reads.

| | New form (sections written as lists of steps) | The form `init` writes (`modules:` and `use:`) |
|---|---|---|
| Documents it reads | `.md`, `.markdown`, `.txt` | Those plus HTML, PDF, Word, Excel, PowerPoint, OCR |
| Search | Keyword | Keyword, vector, hybrid, rerank, user dictionary |
| Versions and reuse, `list`, `results`, `optimize`, `release` | Yes | — |
| `serve` (MCP and HTTP), `ask` | Not yet | Yes |

Wiki and PARA are a separate mechanism from both forms and always available. The full difference between the two forms is in the section 10 of [Improve search by numbers](docs/guide.en.md#10-what-still-uses-the-earlier-flow).
