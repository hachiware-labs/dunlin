# Knowledge-Nest

[日本語](README.md)

**Turn the documents you have into knowledge an AI can use and cite. A few commands, and no programming.**

Hand Knowledge-Nest a folder of documents (Markdown, PDF, Word, Excel, web pages ...) and it puts them in a form an AI can use. Start from one of the three things below, by what you want to do. Your documents and index stay in a folder on your machine, and answers and pages always carry their sources.

## Start from what you want to do

| What you want | How to start | AI model | Where you see and use it |
|---|---|---|---|
| [**Let Claude look things up in my documents**](#let-claude-look-things-up-in-your-documents) | `nest init mcp` | Not needed | Agents such as Claude Desktop and Claude Code |
| [**Build a knowledge base people can read from my documents**](#build-a-knowledge-base-from-your-documents-wiki) | `nest init wiki` | Needed (Ollama or an API) | A browser. Agents can look it up too |
| [**Sort piles of documents and notes by the work they serve**](#sort-documents-and-notes-by-the-work-they-serve-para) | `nest init para` | Needed (Ollama or an API) | A browser. Agents can look it up too |

If unsure, start with the first: it needs no model. You can make the other forms from the same documents later. See [Install](#install); a step-by-step walkthrough is in [Getting started](docs/getting-started.en.md).

### Let Claude look things up in your documents

Agents such as Claude find and read your manuals and design documents themselves, and answer with sources. Knowledge-Nest needs no AI model for this. Japanese search works on your machine right away, and your documents stay there. The source here is [the pump maintenance procedure](examples/manuals/maintenance.md) (in Japanese; translated here).

```text
You      When do we inspect the pump again?
Claude   On the first business day of every month. Before inspecting, stop the pump
         and make sure the power is off.
         Sources: maintenance.md line 3, line 5
```

```sh
nest init mcp ./kb --source ./manuals          # make it from a folder of documents
cd kb
nest build                                     # read the documents and build the index
nest search "ポンプの点検はいつ実施しますか"   # try a search
nest serve --print-config                      # print what to paste into the agent's settings
```

Paste what the last command prints into the MCP settings of Claude Desktop or Claude Code, and you are done. After you edit the documents, run `nest build` again: only the changed documents are processed, and a running agent sees the new content from its next question. [MCP guide](docs/start-mcp.en.md)

**Compared with other ways**

- **Rather than letting the agent read the folder itself.** PDF, Word, Excel and scanned documents are searchable by their Japanese words too. Only the passages that answer come back, with document and line, within a set number of characters, so thousands of documents do not fill the agent's context.
- **Rather than a vector-only search server.** Japanese keyword search, strong where you need the exact word (part numbers, clause numbers, technical terms), runs without an embedding model. Add vector search, which handles rephrasing, for a hybrid.
- **Check by numbers whether it finds the answers.** Register questions with the quotes that answer them, and measure the share of evidence search finds. Compare a changed setting side by side and keep the better one.

**When your own app or chatbot should get answers with sources**, `nest init rag` adds an AI model that answers on top of the same search. `nest ask "question"` and HTTP return answers with sources. Unlike building it with LangChain and the like, there is no code to write. The HTTP API takes the OpenAI SDK as it is, so an existing app only changes where it connects. A question with no evidence in the documents is declined rather than answered. [RAG guide](docs/start-rag.en.md)

### Build a knowledge base from your documents (Wiki)

An AI reads scattered manuals and minutes and writes them up as pages by subject. Each document you add extends the pages it concerns. People read the pages in a browser, and agents can look up the same pages.

```text
Pump maintenance
  Inspect the pump on the first business day of every month. Before inspecting, stop the pump
  and make sure the power is off.
  Replace the mechanical seal every two years.
  Sources: maintenance.md
```

```sh
nest config set llm --provider ollama --model qwen3:4b   # choose the AI that writes pages (once)
nest init wiki ./wiki --purpose "Collect the maintenance procedures and parts information"
cd wiki
nest ingest ../manuals   # ingest documents; pass a URL for a web page
nest serve               # open http://127.0.0.1:4319 in a browser
```

**Compared with other ways**

- **Rather than a tool that only has an AI summarize.** Each page keeps the documents it was written from as its sources, and a program checks that its numbers, dates and quotations are in them. When a document gets a new version, `nest lint` points out the pages written from the old one.
- **Knowledge stays as files on your machine.** Pages are HTML files in `wiki/`, originals are files in `raw/`. They open without a server and can go to other tools. Each change the AI makes is recorded as a job and `nest undo` takes it back; originals are never changed.
- **People and agents use the same pages.** People read in a browser and instruct a page, such as "list the checks before inspection as bullets". Agents look up the same pages over MCP.
- **Pages behind a sign-in can be ingested.** Sign in once in nest's own browser, and internal pages drawn by scripts, or posts on X, are taken in as originals.

It follows Andrej Karpathy's LLM-Wiki idea ([what an LLM-Wiki is](#what-an-llm-wiki-is)). [Wiki guide](docs/start-wiki.en.md)

### Sort documents and notes by the work they serve (PARA)

Use it when documents and notes have piled up and you no longer know what is where. An AI writes a note for each document and puts it in one of the four places of Tiago Forte's PARA. PARA sorts by whether you use something in your work now, not by subject ([explanation](https://fortelabs.com/the-p-a-r-a-method-a-universal-system-for-organizing-digital-information-75a9da8bfb37/)).

| Place | What goes there | Example |
|---|---|---|
| Projects | Efforts with a deadline | Next month's pump replacement |
| Areas | Ongoing responsibilities | Routine equipment maintenance |
| Resources | Reference that may help someday | A parts maker's catalog |
| Archives | What is finished | Last year's inspection records |

You list your projects and areas in `rules.md`. The AI picks, from that list, where each document will be used, and writes its key points and how to use it in a note. It starts as the Wiki does, with `init para` in place of `init wiki`. [PARA guide](docs/start-para.en.md)

**Compared with other ways**

- **Rather than filing by hand in a notes app.** Put documents in, and the AI writes how each serves a project in a note and puts it where it will be used, with a copy of the document beside it.
- **No folders appear on their own.** The AI can only use the projects and areas you listed in `rules.md`. A note aimed anywhere else is not saved, and you are told.
- **Put away a finished project in one step.** `nest archive` moves its notes, document copies, links from other notes and its line of `rules.md` to Archives together; `nest undo` brings them back. `nest lint` points out projects past their deadline.

## What all of them share

- **No programming.** You use the `nest` commands and a browser screen. What is done and how is written out to `KnowledgeNestfile.yaml`, which you touch only when you want to change something.
- **Stays on your machine.** Documents and indexes are kept in the project folder. The only thing sent out is the text needed for a model you set up yourself.
- **Always with sources.** Search results, answers and pages all point back to the original documents.
- **Agents can use it too.** Every form is served over MCP, so agents such as Claude use the same knowledge.

A table comparing it with the usual other ways, for each thing you want to do, and when another way suits better, is in [Where Knowledge-Nest fits](docs/comparison.md) (Japanese).

## How the four forms differ

This part is for those who want the details. If the table above was enough to choose, skip it.

### At a glance

| | MCP | RAG | Wiki | PARA |
|---|---|---|---|---|
| What you get | Search for agents | An API that answers with sources | Pages of knowledge with sources | Notes in PARA form |
| Who uses it | Agents | Your app | People and agents | People and agents |
| When documents are read | At each question | At each question | At ingest | At ingest |
| What an agent receives | Passages of the documents | Passages of the documents (`/mcp`) | Pages the AI wrote | Notes the AI organized |
| AI model | Not needed | Needed to answer | Needed to write | Needed to write |
| Updating documents | `build`, changed documents only | `build`, changed documents only | `ingress` adds to pages | `ingress` adds notes |
| Served as | MCP | HTTP (answers, search) and MCP | Web (pages and a work screen) and MCP | Web (notes and a work screen) and MCP |

MCP and RAG search the documents at each question, so even many or often-changing documents are reflected at once. Wiki and PARA have an AI read and organize the documents first; the result is knowledge people can read as it is.

### How MCP and RAG differ

The difference is **who writes the answer from the evidence found**.

```text
MCP   question ─→ [agent] ─asks for a search─→ Knowledge-Nest ─→ evidence (passages)
                     ↑                                            │
                     └──────── the agent's model reads it and answers ──┘

RAG   question ─→ Knowledge-Nest ─→ search ─→ evidence ─→ a model Knowledge-Nest calls writes the answer ─→ answer with sources
```

- **MCP** is a librarian who brings the documents to the person looking things up. The agent rephrases and searches as often as it needs. Results carry document, line and heading, and `knowledge.read` reads back the original text.
- **RAG** is a librarian who reads the documents and hands over a written answer. One question in, one answer out. Responses take the shape of OpenAI's Responses API, and `/v1/responses`, `/v1/search` and `/mcp` are served on one port. Token authentication and connection limits are available when you open it to others.
- RAG's `serve` also serves MCP (`/mcp`), so moving from MCP to RAG keeps agents' search working.

Japanese is segmented with Lindera and IPADIC, and documents that match only on light words such as "する" or "こと" are not returned. Add technical terms with a user dictionary. Hybrid search with vectors, PDF, Office, HTML and OCR are supported too. [MCP and RAG](docs/mcp.md) (Japanese)

### What an LLM-Wiki is

LLM-Wiki is an approach to building a knowledge base with an LLM, proposed by Andrej Karpathy ([the original gist](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f)).

RAG searches the documents for fragments at each question and builds an answer on the spot; nothing it read is kept. In an LLM-Wiki, the LLM writes what it read as linked Wiki pages, and extends the pages a new document concerns. Knowledge becomes something written once and grown. People choose the documents and ask the questions.

| LLM-Wiki operation | What it does | Knowledge-Nest |
|---|---|---|
| Ingest | Read a document and write or revise the pages it concerns | `nest ingest` |
| Query | Ask the Wiki's pages, not the original documents | `nest query` |
| Lint | Find orphan pages, broken links, contradictions and stale statements | `nest lint` (sources, versions, numbers and quotations, links), `--content` (contradictions between pages) |

Text an LLM writes can contain plausible errors. Knowledge-Nest covers this weakness by checking against sources. PARA shares with the Wiki the checks against sources, originals that are never changed, undoable jobs, and serving on the web and over MCP. [Using Wiki and PARA](docs/wiki.md) (Japanese)

## Improve search by numbers (new commands, in development)

We are building a way to keep search data as versions, and to measure and compare many candidate settings at once.

```sh
nest eval-data import cases.json   # bring in pairs of a question and the quote that answers it
nest eval                          # measure recall with the current settings
nest optimize tune.yaml            # make candidates from an instruction that says what to vary, and measure each
nest results tune                  # list the candidates, best recall first
nest release tune/010              # give the chosen index a served name
```

An example of what `results` prints (some columns left out):

```text
Trial  Recall  To base  What
010    1.000   +0.333   chunking=length(size=200),search=top10
000    0.667   -        base
```

Rebuilding, evaluating, and optimizing never change the index being served. It switches only when you check and `release` (to switch on every build, use `nest build --release`).

For now this supports text and Markdown with keyword search, and `serve` does not yet serve its indexes. The steps and the details are in [Improve search by numbers](docs/guide.en.md).

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
| Compare with other tools | [Where Knowledge-Nest fits](docs/comparison.md) · [Benchmarks](docs/benchmarks.md) |
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
