# Dunlin

[日本語](README.md)

**Hand over a folder of documents, and AI can answer from it — and show where the answer came from.**

Dunlin builds a document database for AI from the documents you already have. From the same folder it makes search that agents query (MCP), an API that answers with sources (RAG), a Wiki that an AI writes and keeps up (LLM-Wiki), or notes that an AI organizes (PARA). Your documents and index stay on your machine, and answers always carry sources.

```text
Folder of documents ──→  Dunlin  ──┬─ MCP    agents such as Claude look things up and answer
(Markdown, PDF,                    ├─ RAG    your app gets answers with sources over an API
 Word, Excel ...)                  ├─ Wiki   an AI reads the documents and writes pages with citations
                                   └─ PARA   an AI sorts documents and notes into the four PARA categories
```

## What it looks like

These are illustrations. The source document is [the pump maintenance procedure](examples/manuals/maintenance.md) (in Japanese; translated here).

**Let Claude look things up in your manuals (MCP)**

```text
You      When do we inspect the pump again?
Claude   On the first business day of every month. Before inspecting, stop the pump
         and make sure the power is off.
         Sources: maintenance.md line 3, line 5
```

**Get answers with sources in your own app (RAG)**

```sh
curl http://127.0.0.1:4318/v1/responses -d '{"input":"How often is the mechanical seal replaced?"}'
# → "Every two years [1]", where [1] cites line 7 of maintenance.md
```

**Let an AI write a Wiki — and keep not one sentence without evidence (Wiki)**

```text
Pump maintenance
  Inspect on the first business day of every month [1]. Stop the pump and cut power first [2].
  [1] maintenance.md line 3  [2] maintenance.md line 5
```

## Try it in three minutes: let an agent search your documents

No model to set up. Japanese keyword search works on your machine right away.

```sh
dunlin init mcp ./kb --source ./manuals
cd kb
dunlin build
dunlin search "ポンプの点検はいつ実施しますか"
dunlin serve --print-config
```

Paste what the last command prints into the settings of an MCP client such as Claude Desktop or Claude Code, and the agent can search your documents. After you edit the documents, run `dunlin build` again; only the changed documents are processed.

See [Install](#install). A step-by-step walkthrough is in [Getting started](docs/getting-started.en.md).

## Which document database to build, and when

### Decide with two questions

```text
Q1 Who mainly reads this knowledge?
   │
   ├─ AI (an agent or an app) looks it up at each question
   │     │
   │     Q2 Who writes the answer?
   │     ├─ The agent itself (Claude, for example) ............ MCP
   │     └─ Dunlin (the app only receives the answer) .. RAG
   │
   └─ People read it as pages, and it grows over time
         │
         Q2 How should it be arranged?
         ├─ As pages of knowledge, by topic ................... Wiki
         └─ By how you work (the four PARA categories) ........ PARA
```

### Choose by situation

| When you… | Build | Why |
|---|---|---|
| Want Claude Code to look up your internal procedures and design documents while it works | **MCP** | The agent rephrases and searches as often as it needs. No model to set up |
| Don't want to hand your documents to an outside model, and want to try everything locally first | **MCP** | Keyword search runs entirely on your machine, and the agent you already use writes the answer |
| Want to add Q&A based on your documents to an internal portal or a chatbot | **RAG** | Send a question over HTTP and get an answer with sources back. The OpenAI SDK works as is |
| Must always show the evidence behind answers to inquiries | **RAG** | Each citation points to the document's version and the range in its text |
| Want a knowledge base that newcomers can read, built from scattered procedures and minutes | **Wiki** | An AI writes pages by topic and puts a citation on every sentence |
| Want to find contradictions between rules and manuals, or statements that went stale | **Wiki** | `verify --content` finds contradictions between pages; `verify` finds stale citations |
| Have research notes and documents piling up and can't find what's where | **PARA** | An AI sorts them into projects, ongoing work, reference, and finished things |
| Want to keep each project's documents apart from the documents of work you own long-term | **PARA** | Projects and Areas are kept apart. Finished projects move to Archives |
| Want people to read it and agents to search it | **Wiki and MCP** | Give the same document folder to two projects, `wiki` and `mcp` |
| Need to explain and improve search accuracy with evidence | **MCP or RAG** | Measure recall with pairs of questions and answers, then compare settings and choose |

### The four forms at a glance

| | MCP | RAG | Wiki | PARA |
|---|---|---|---|---|
| What you get | Search for agents | An API that answers with sources | Pages of knowledge with citations | Notes in the PARA method |
| Mainly used by | Agents | Your app | People (agents via `query`) | People (agents via `query`) |
| When documents are read | At each question | At each question | At ingest | At ingest |
| Model | Not needed | Needed to answer | Needed to write | Needed to write |
| Updating documents | `build`; only changed documents | `build`; only changed documents | `ingress`; adds to the pages | `ingress`; adds to the notes |
| How it is served | MCP (stdio) | HTTP (answers, search, MCP) | A browser screen | A browser screen |

- **Model effort**: only MCP needs no model on Dunlin's side. RAG calls a model at each question; Wiki and PARA call one at each ingest.
- **Not sure? Start with MCP.** One document folder is enough to try it. If you later build RAG or a Wiki from the same documents, MCP keeps working.

### How MCP and RAG differ

Both search your documents at each question. The difference is **who writes the answer from the evidence found**.

```text
MCP   question ─→ [agent] ─asks for a search─→ Dunlin ─→ evidence (the passages)
                     ↑                                       │
                     └── the agent's own model reads the evidence and answers ──┘

RAG   question ─→ Dunlin ─→ search ─→ evidence ─→ a model Dunlin calls writes the answer ─→ answer with sources
```

| | MCP | RAG |
|---|---|---|
| An analogy | A librarian brings the right documents to the person doing the research | A librarian reads the documents and hands over a written answer |
| What Dunlin returns | Evidence (the passages and their sources) | An answer with sources |
| Who writes the answer | The model on the agent's side (Claude, for example) | A model that Dunlin calls (Ollama or an API) |
| The caller | An agent or app that speaks MCP | Anything that can call HTTP (your own app, the OpenAI SDK) |
| A model on Dunlin's side | Not needed | An answering model is needed (search works without one) |
| How questions go | The agent rephrases and searches as often as it needs | You send one question and get one answer back |

- **You want Claude or another agent to research your documents** → MCP.
- **You want your app to receive answers** → RAG.
- **Not sure** → start with MCP. It needs no model. The `serve` of a RAG project also offers MCP (`/mcp`) on the same port, so agents keep their search if you move to RAG later.

### MCP: let Claude search your documents

- **All it takes is a folder.** No GPU, no embedding model: ask in plain Japanese and it finds the passages. Your documents never leave your machine.
- **Sources down to the line.** Each result carries the document, lines, and heading, and the agent can read the original back with `knowledge.read`.
- **Connecting is copy and paste.** Paste the output of `dunlin serve --print-config` into your MCP client's settings.
- **Keeps up with your documents without stopping.** Run `dunlin build` again: only the changed documents are processed, and running search uses the new index from the next question.

Japanese is split into words with Lindera and IPADIC, and documents that match only on light words such as "する" or "こと" are not returned. Add specialist terms with a user dictionary. Hybrid search with vectors, and PDF, Office, HTML, and OCR, are supported too. [MCP and RAG](docs/mcp.md) (Japanese)

### RAG: an API that answers your app with sources

- **The OpenAI SDK just works.** Responses have the shape of the OpenAI Responses API. Point `baseURL` at `http://127.0.0.1:4318/v1` and you are done.
- **Citations point into the original.** Each citation carries the version of the document and the range in its text.
- **Answers, search, and MCP on one port.** `/v1/responses`, `/v1/search`, and `/mcp` are served together.
- **Local model or API, your choice.** Use a local Ollama model or an external API. To expose it, set token authentication and connection limits.

```sh
dunlin config set llm --provider ollama --model qwen3:4b
dunlin init rag ./rag --source ./manuals
cd rag
dunlin build
dunlin ask "ポンプの点検周期は？"
dunlin serve
```

[MCP and RAG](docs/mcp.md) (Japanese)

### Wiki: an AI writes, and nothing without evidence survives

- **Knowledge grows as pages.** Instead of searching the documents again at every question, an AI reads them and writes tidy pages. Each new document makes the pages grow.
- **Every sentence the AI writes is checked.** Statements carry citations, and a statement whose citation does not match the source word for word is not saved.
- **Everything can be undone.** Originals are never rewritten. Every change is recorded as a job, and `dunlin undo` takes it back.
- **Finds what went stale.** `dunlin verify` finds broken links and stale citations; with `--content` it also finds contradictions between pages.
- **Read and ask in a browser.** Read documents, pages, and sources side by side, and ask for changes such as "list the checks before inspection as bullets".

```sh
dunlin config set llm --provider ollama --model qwen3:4b
dunlin init wiki ./wiki --purpose "Collect maintenance procedures and part information"
cd wiki
dunlin ingress ../manuals
dunlin serve        # open http://127.0.0.1:4319 in a browser
```

Agents can get answers based on the pages with `dunlin query "question" --json`, and read the pages as HTML in the project's `site/`. The Wiki's `serve` is a browser screen for people, not an MCP server; if you want agents to search over MCP, make an `mcp` project from the same documents too. [Wiki and PARA](docs/wiki.md) (Japanese)

### PARA: an AI organizes your documents and notes

PARA sorts information into four categories by what you use it for now.

| Category | What goes in | Example |
|---|---|---|
| Projects | Efforts with a deadline | Next month's pump replacement work |
| Areas | Ongoing responsibilities | Routine equipment maintenance |
| Resources | Reference that may help someday | A parts maker's catalog |
| Archives | Finished things | Last year's inspection records |

- **Leave the sorting to the AI.** When you ingest documents, the AI writes notes and files them under a category.
- **The same safety as the Wiki.** Citation checks, storage that never rewrites originals, and undoable jobs work the same way.

Read `init wiki` as `init para ./para` and you are started.

## Why Dunlin

- **Everything stays local.** Documents and index live in the project folder. The only text sent anywhere is what a model needs, and only when you configure that model.
- **Always sourced.** Search results, answers, and Wiki statements all point back to the place in the original.
- **Improve by numbers, not by feel.** Register pairs of "this question, this sentence answers it", and measure the share of questions whose evidence search finds (recall). Change a setting, measure again, compare, then choose (MCP and RAG).
- **Trying is fast.** The same input and the same processing never run twice. Changing only the chunk size does not read the documents again, so you can try many variations.
- **The configuration is visible.** What Dunlin does is written to `Dunlinfile.yaml`, and `dunlin validate` checks it before you run.

How Dunlin compares with LangChain or a vector database is in [Where Dunlin fits](docs/comparison.md) (Japanese).

## Improve search by numbers (new commands, in development)

We are building a way to keep search data as versions, and to measure and compare many candidate settings at once.

```sh
dunlin eval-data import cases.json   # bring in pairs of a question and the quote that answers it
dunlin eval                          # measure recall with the current settings
dunlin optimize tune.yaml            # make candidates from an instruction that says what to vary, and measure each
dunlin results tune                  # list the candidates, best recall first
dunlin release tune/010              # give the chosen index a served name
```

An example of what `results` prints (some columns left out):

```text
Trial  Recall  To base  What
010    1.000   +0.333   chunking=length(size=200),search=top10
000    0.667   -        base
```

For now this supports text and Markdown with keyword search, and `serve` does not yet serve its indexes. The steps and the details are in the [Guide](docs/guide.en.md).

## Install

Publishing to npm is being prepared. For now, build from source. You need Rust 1.95 or later and Node.js 22.20 or later in the 22 line, or 24.12 or later in the 24 line.

```sh
git clone https://github.com/hachiware-labs/dunlin
cd dunlin
npm ci
npm run build
npm link
dunlin --help
```

## Documentation

Pages without an English version are in Japanese.

| Goal | Page |
| --- | --- |
| Get started | [Getting started](docs/getting-started.en.md) · [Quickstart](docs/quickstart.md) |
| Use it well | [Guide](docs/guide.en.md) |
| Search, answers, and pages | [MCP and RAG](docs/mcp.md) · [Wiki and PARA](docs/wiki.md) |
| Improve search | [Evaluation and dictionary](docs/evaluation.md) · [Agents and trials](docs/agents.md) · [Techniques that could raise accuracy](docs/retrieval-techniques.md) |
| Look up commands and settings | [CLI](docs/cli.md) · [Modules](docs/modules.md) · [Dunlinfile defaults](docs/config.md) · [Troubleshooting](docs/troubleshooting.md) |
| Understand how and where it fits | [Where Dunlin fits](docs/comparison.md) · [Data management design](docs/data-management.md) · [Builds and evaluation](docs/concepts.md) · [Architecture](docs/architecture.md) · [Benchmarks](docs/benchmarks.md) · [PDF extraction](docs/pdf-extraction.md) |
| Embed in Node.js | [Node.js API](docs/api.md) |
| Develop and operate | [Contributing](CONTRIBUTING.md) · [Changelog](CHANGELOG.md) · [Security](SECURITY.md) · [Acceptance checks](docs/acceptance.md) |
| Licenses and notices | [License](LICENSE) · [Third-party notices](THIRD_PARTY_NOTICES.md) |
| Specification | [Dunlin design specification](docs/dunlin-spec.md) |

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
- `dunlin` is a placeholder package name. Publishing to npm is being prepared. The license is MIT.
- Tested environments: Windows x64, Linux x64 (glibc), and macOS arm64 and x64.
- The versioned store and its commands (`list`, `results`, `optimize`, `release`) are being implemented on the `data-management` branch. A command picks its mechanism by the form of the configuration file it reads.

| | New form (sections written as lists of steps) | The form `init` writes (`modules:` and `use:`) |
|---|---|---|
| Documents it reads | `.md`, `.markdown`, `.txt` | Those plus HTML, PDF, Word, Excel, PowerPoint, OCR |
| Search | Keyword | Keyword, vector, hybrid, rerank, user dictionary |
| Versions and reuse, `list`, `results`, `optimize`, `release` | Yes | — |
| `serve` (MCP and HTTP), `ask` | Not yet | Yes |

Wiki and PARA are a separate mechanism from both forms and always available. The full difference between the two forms is in the [appendix of Getting started](docs/getting-started.en.md#appendix-the-two-forms-of-configuration).
