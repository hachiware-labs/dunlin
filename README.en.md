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

## Every database, with little effort

It does not just make them all. For every form, Knowledge-Nest takes on the chores that building the same thing with other libraries leaves to you. The comparison, including when another way suits better, is in [Where Knowledge-Nest fits](docs/comparison.md) (Japanese).

### MCP: search that agents query

What MCP does well is **grow the search to fit your documents and how they are used**. It is not done once loaded: you measure it, and make it find more.

| How it grows | What happens | What does it |
|---|---|---|
| Documents are added or changed | Only the changed documents are processed; a running agent sees the new content from its next question, with nothing stopped | `nest build` |
| Measure how well it finds answers | Collect questions with the quotes that answer them, and measure the share of evidence search finds. An AI drafts candidate questions, and people check them before adding | `nest eval-data generate` and `review`, `nest eval` |
| Teach it your words | From missed questions, suggest technical terms that get split apart, and add them to a user dictionary | `nest dictionary suggest` and `add` |
| Improve the setup | Try chunk sizes, hybrid search, a reranker and more, and compare with earlier results. Trying candidates at once optimizes it too | `nest report compare`, `nest optimize` |
| Switch safely | Trying never changes the search being served; only a result you checked is switched to | `nest use`, `nest release` |

**With other MCP servers** (filesystem or vector database MCP servers), you do this yourself:

1. Find a tool that turns PDF and Office into text an agent can read
2. Chunk the documents, pick an embedding model, and load a vector database
3. Load them again every time the documents change
4. Feel your way when part numbers or technical terms are missed

**With Knowledge-Nest**

```sh
nest init mcp ./kb --source ./manuals
cd kb
nest build
nest serve --print-config   # paste what it prints into the agent's settings
```

It reads PDF, Office and scanned documents, and searches them by Japanese words with no embedding model. After you edit the documents, `nest build` replaces only the ones that changed. Measure how well it finds answers by recall, and optimize by trying candidate settings at once. [MCP guide](docs/start-mcp.en.md)

### RAG: an API that answers with sources

What RAG does well, too, is **keep growing what its answers stand on**. An answer is only as good as the evidence found, and RAG's search is the same as MCP's, so the growth above becomes the quality of the answers. On top of that, the answering side can widen.

| How it grows | What happens | What does it |
|---|---|---|
| The search grows | Document updates, measuring, the user dictionary, and optimizing the setup, as for MCP | As for MCP |
| Change the model that writes answers | Swap a local Ollama for an API model by changing the settings and restarting `serve`, with no index to rebuild | `models` in `KnowledgeNestfile.yaml` |
| Serve more apps | Open the same API to other internal apps and chatbots; agents use `/mcp` on the same port. Add authentication and limit who connects | `nest serve`, `serve.access` |

**With other libraries** (LangChain, LlamaIndex and the like), you do this yourself:

1. Pick parts for loading, chunking, embedding, a vector database, search and generation, and wire them in code
2. Work out how answers cite sources, and how to decline when there is no evidence
3. Stand up an API server for your apps, and add authentication
4. Combine a separate evaluation library to measure how good the answers are

**With Knowledge-Nest**

```sh
nest config set llm --provider ollama --model qwen3:4b   # once
nest init rag ./rag --source ./manuals
cd rag
nest build
nest serve   # an API the OpenAI SDK calls as it is
```

Answers cite the version of the document and the range in the original, and a question with no evidence is declined. Authentication and limits on who connects are a few lines of settings. Drafting questions, measuring and optimizing are all in the same tool. [RAG guide](docs/start-rag.en.md)

### Wiki: knowledge pages everyone uses

What the Wiki does well is **grow a team's knowledge into a shared asset that belongs to no one person**. The documents and field knowledge each member has gather into pages by subject, and build up with their sources kept.

| How it grows | What happens | What does it |
|---|---|---|
| Someone adds a document | The pages it concerns are extended, and a new subject becomes a page. A document with nothing new is recorded as such and does not pad the Wiki | `nest ingest`, or **資料を取り込む** (add documents) on the screen |
| A document is revised | Only the pages written from it are rewritten from the new version | Tracking document versions |
| People add what they know | Field knowledge in no document becomes a source, with who said it, and goes into the page | `nest instruct <page> "..." --supplement "..." --by <name>` |
| Someone asks a question | An answer worth keeping becomes a page for the next person | `nest query`, `nest save-answer` |
| Gaps and conflicts are found | Contradictions between pages, terms with no page, and questions and documents to look for next are named | `nest lint --content` |
| Everyone uses it | The browser screen, opened to the team's network, and agents over MCP use the same pages | `nest serve` |

People do not edit pages directly; they instruct the AI to change them. So whoever works on a page, it keeps its sources. What was done and when is kept in the log of the work (`log.html`), and each job can be undone.

**With other tools** (LLM-Wiki scripts, notes app AI and the like), you do this yourself:

1. Build the way an AI reads documents and writes pages, and the rules for writing them
2. Read every page the AI wrote and check it against the documents
3. Find which pages to fix every time a document is revised
4. Provide a screen for people and a way in for agents, separately

**With Knowledge-Nest**

```sh
nest init wiki ./wiki --purpose "Collect the maintenance procedures and parts information"   # model set as for RAG
cd wiki
nest ingest ../manuals
nest serve   # read in a browser; agents look it up over MCP
```

A program checks that numbers, dates and quotations are in the sources, and points out contradictions between pages and terms with no page. A revised document gets only its own pages rewritten. Each change the AI makes can be undone. It is Karpathy's LLM-Wiki idea, with checks against sources added. [Wiki guide](docs/start-wiki.en.md)

### PARA: notes that organize your own work

What PARA (Tiago Forte's method) does well is **grow the knowledge of your work and the knowledge of yourself apart**.

| Place | Knowledge it grows | Example | Who decides |
|---|---|---|---|
| **Projects** | Knowledge for current work and themes; it has a deadline and is put away when done | Next month's pump replacement | You (in `rules.md`) |
| **Areas** | Knowledge for work you stay responsible for | Routine equipment maintenance | You (in `rules.md`) |
| **Resources** | Knowledge of your own skills and interests; it stays and builds up when your work changes | Vibration diagnosis, trends among parts makers | The AI, adding topics as documents come |
| **Archives** | Finished projects, and areas you no longer own | Last year's replacement | Moved with `nest archive` |

Documents for your work gather as notes of that project, and the whole project goes to Archives when it ends. Documents you do not use in current work but that feed your own growth collect under topics in Resources, and keep growing as your work changes. In Knowledge-Nest you set the frame of your work (Projects and Areas), and the AI widens the frame of your interests (Resources) as documents come.

**Running PARA in a notes app** (Notion, Obsidian and the like), you do this yourself:

1. Read each new document, decide which project it serves, and file it
2. Write a note on how to use it, and keep the original beside it
3. When a project ends, move its notes, documents and links to Archives by hand
4. Keep watch for projects past their deadline

**With Knowledge-Nest**

```sh
nest init para ./para --purpose "Organize documents and notes for my equipment maintenance work"
cd para
# list your current projects and areas in rules.md
nest ingest ../inbox                      # the AI writes how each serves a project, and files it there
nest archive Projects/pump-replacement    # when it ends, notes, documents and links go away at once
```

Notes go only to the projects and areas you listed, so no folders appear on their own. `nest undo` brings an archived project back, and `nest lint` points out projects past their deadline. [PARA guide](docs/start-para.en.md)

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
