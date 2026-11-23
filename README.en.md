# Knowledge-Nest

[日本語](README.md)

**Turn the documents you have into knowledge an AI can use and cite. A few commands, and no programming.**

Hand Knowledge-Nest a folder of documents (Markdown, PDF, Word, Excel, web pages ...) and it puts them in a form an AI can use. Your documents and index stay in a folder on your machine, and answers and pages always carry their sources.

**One tool makes four kinds of document database.** Search that agents query (MCP), an API that answers with sources (RAG), knowledge pages everyone uses (Wiki), and notes that organize your own work (PARA). Where you would usually pick and combine a vector database, a RAG framework, a wiki tool and a notes app, Knowledge-Nest alone makes them all.

```text
                                         ┌─ MCP    search that agents query
Folder of documents ──→ Knowledge-Nest ──┼─ RAG    answers with sources, returned to your app
(Markdown, PDF, Word, Excel ...)         ├─ Wiki   knowledge pages everyone uses
                                         └─ PARA   notes that organize your own work
```

- **One thing to learn.** One install, and one way to start: `nest init <form>`. Loading documents, citing sources, the configuration file (`KnowledgeNestfile.yaml`) and serving with `nest serve` are the same for every form.
- **Grow from the same documents as needs change.** Start with search on a folder of documents, then add the answer API or a Wiki on the same folder later, with nothing to rebuild or migrate.
- **Agents can reach every form.** All four are served over MCP, so agents such as Claude use the documents themselves and the knowledge an AI wrote from them in the same way.

Start from one of the four things below, by what you want to do.

## Start from what you want to do

| What you want | How to start | AI model | Where you see and use it |
|---|---|---|---|
| [**Let Claude look things up in my documents**](#let-claude-look-things-up-in-your-documents) | `nest init mcp` | Not needed | Agents such as Claude Desktop and Claude Code |
| [**Return answers based on my documents in a chat app or internal tool**](#return-answers-based-on-your-documents-in-a-chat-app-rag) | `nest init rag` | Needed to answer (Ollama or an API) | Your own app or chatbot (HTTP, callable with the OpenAI SDK) |
| [**Write up my documents as knowledge pages everyone uses**](#write-up-documents-as-a-knowledge-base-everyone-uses-wiki) | `nest init wiki` | Needed to write (Ollama or an API) | A browser. Agents can look it up too |
| [**Organize the documents and notes of my own work by my projects**](#organize-your-own-documents-and-notes-by-project-para) | `nest init para` | Needed to write (Ollama or an API) | A browser. Agents can look it up too |

- **Search, or answer.** The first two search the documents at each question. MCP lets an agent look things up and answer itself; RAG returns the words of the answer to your app.
- **Both organize documents, for different ends.** The last two have an AI read and organize the documents first. The Wiki builds knowledge everyone uses, arranged by what it is about. PARA helps you get your own work done, arranged by which project a document serves. Shared team knowledge goes in a Wiki; the documents of your own work go in PARA.

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
- **Measured, and optimized.** Register questions with the quotes that answer them (an AI can draft the questions too), and measure the share of evidence search finds. Try candidate settings, such as chunk sizes or the number of results, all at once, rank them, and switch serving only to the one you checked. With search you build yourself, you would set up this evaluation and tuning separately. [Measure and optimize search](#measure-and-optimize-search-mcp-and-rag)

### Return answers based on your documents in a chat app (RAG)

Have an internal chatbot, a help desk or your own app return answers written from your documents. It is the search above with an AI model added that writes the answer. The app only sends the question and gets back an answer with sources.

```sh
curl http://127.0.0.1:4318/v1/responses -d '{"input":"How often is the mechanical seal replaced?"}'
# → "Every two years [1]", where [1] cites line 7 of maintenance.md
```

```sh
nest config set llm --provider ollama --model qwen3:4b   # choose the AI that writes answers (once)
nest init rag ./rag --source ./manuals
cd rag
nest build
nest ask "ポンプの点検周期は？"   # try a question
nest serve                        # serve answers, search and MCP at http://127.0.0.1:4318
```

[RAG guide](docs/start-rag.en.md)

**Compared with other ways**

- **Rather than building it with LangChain and the like.** No code to write: a configuration file and commands bring it up. The answer API takes the OpenAI SDK as it is, so an existing app only changes where it connects.
- **No evidence, no answer.** A question with no evidence in the documents is declined rather than answered. An answer's sources point to the version of the document and the range in the original.
- **Rather than a no-code RAG app.** Documents and index stay in a folder on your machine. With a local Ollama as the model, documents never leave it. When you open it to other machines, add authentication and limit who connects.
- **Measure and optimize what answers stand on.** An answer is only as good as the evidence found. The search is the same as MCP's, so measure recall and optimize the settings.

### Write up documents as a knowledge base everyone uses (Wiki)

Use it to build knowledge a team or department shares. An AI reads scattered manuals and minutes and writes them up as pages by subject, arranged by what the knowledge is about so that anyone can follow it. Each document you add extends the pages it concerns. People read the pages in a browser, and agents can look up the same pages.

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

Have a chat summarize your documents, and the answer is gone when the chat ends. Have a notes app's AI write, and you no longer know what came from the documents. Knowledge-Nest's Wiki piles up what it read as pages with sources, and follows the documents when they change.

- **Knowledge piles up.** A new document extends the pages it concerns. A document with nothing new is recorded as having nothing to add, and does not pad the Wiki. An answer worth keeping becomes a page with `nest save-answer`.
- **When a document is revised, only its pages are rewritten.** Ingest a new version, and the AI rewrites just the pages written from that document. `nest lint` points out pages not yet brought up to date.
- **A program doubts what the AI wrote.** At every ingest, the numbers, dates and quotations of a page are checked against its sources. `nest lint --content` names contradictions between pages, terms with no page, and even questions to look into next and documents to look for.
- **What people know is kept as a source too.** Field knowledge that is in no document, such as "we actually replace it every three months", is recorded with who said it, and the page is fixed with it as a source. Each change the AI makes is recorded as a job, and `nest undo` takes it back. Originals are never changed.
- **Knowledge stays as files on your machine.** Pages are HTML files in `wiki/`, with a contents page (`index.html`) and a log of the work (`log.html`). They open without a server and move with the folder. People use the pages in a browser, and agents the same pages over MCP.
- **Collect from pages behind a sign-in, and from X.** Sign in once in nest's own browser, and internal pages can be ingested. X bookmarks come in one post at a time, with the pages they link to and the posts they quote.

It follows Andrej Karpathy's LLM-Wiki idea ([what an LLM-Wiki is](#what-an-llm-wiki-is)). [Wiki guide](docs/start-wiki.en.md)

### Organize your own documents and notes by project (PARA)

Use it when the documents and notes of your own work have piled up and you no longer know what is where. Where the Wiki arranges knowledge for everyone by subject, PARA arranges it by whether you use it in your own projects. An AI writes a note for each document and puts it in one of the four places of Tiago Forte's PARA. PARA sorts by whether you use something in your work now, not by subject ([explanation](https://fortelabs.com/the-p-a-r-a-method-a-universal-system-for-organizing-digital-information-75a9da8bfb37/)).

| Place | What goes there | Example |
|---|---|---|
| Projects | Efforts with a deadline | Next month's pump replacement |
| Areas | Ongoing responsibilities | Routine equipment maintenance |
| Resources | Reference that may help someday | A parts maker's catalog |
| Archives | What is finished | Last year's inspection records |

You list your projects and areas in `rules.md`. The AI picks, from that list, where each document will be used, and writes its key points and how to use it in a note. It starts as the Wiki does, with `init para` in place of `init wiki`. [PARA guide](docs/start-para.en.md)

**Compared with other ways**

PARA takes effort to keep up. Each new document needs a decision about which project it serves, and each finished project needs putting away; that effort is where PARA tends to fall apart. Knowledge-Nest hands that effort to the AI and to commands, and leaves setting the frame to you.

- **Not just filed, but with how to use it.** A note says how this document serves this project. Unlike filing that stops at a folder, you can tell later why it is there.
- **You set the frame, the AI keeps to it.** The AI can only use the projects and areas you listed in `rules.md`. A note aimed anywhere else is not saved, and its document stays pending; list the project and run `nest ingress --pending` to place it.
- **A project's folder is ready to use.** A copy of each document sits in `資料/` beside its note, so opening a project's folder gives you the notes and the originals together.
- **Put away in one step, and bring back any time.** `nest archive` moves a project's notes, document copies, links from other notes and its line of `rules.md` to Archives together; `--restore` or `nest undo` brings them back. `nest lint` points out projects past their deadline.
- **Agents look things up in the context of your current work.** Notes are served over MCP too, so ask Claude "which parts do we replace in next month's work?" and it answers from that project's notes. Checks against sources, undo and the browser screen are shared with the Wiki.

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
| Who uses it | Agents | Your app | A team, and agents | You, and agents |
| Why organize | — | — | To build knowledge everyone uses | To get your own work done |
| Arranged by | — | — | What the knowledge is about (subject) | Which project it serves (Projects, Areas, Resources, Archives) |
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

## Measure and optimize search (MCP and RAG)

"Would smaller chunks find more?" is decided by numbers, not by a hunch. Pairs of a question and the quote that answers it measure the share of evidence search finds (recall). Write only what to vary, and candidate settings are made and measured in turn. Candidates that share their first steps reuse what was made, so trying many stays fast.

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

There are tools for reading the misses too. An evaluation shows where evidence was lost (in search, in fusion, or over the context budget) and how each kind of question did. `nest eval-data generate` has an AI draft candidate questions, which you check before use.

`optimize` and `release` belong to a new mechanism in development: for now they support text and Markdown with keyword search, and `serve` does not yet serve their indexes. Settings with vector search or reranking are compared one at a time with `nest eval` and `nest report compare`. The steps and the details are in [Improve search by numbers](docs/guide.en.md) and [Evaluation and dictionary](docs/evaluation.md) (Japanese).

## Rearrange it, and add what is missing (MCP and RAG)

MCP and RAG are built from modules listed by stage (loading, chunking, index, search, answer) in a configuration file, `KnowledgeNestfile.yaml`. Rearrange them by changing the list and its arguments, without programming.

- **Rearrange.** Adding vector search and a reranker to keyword search, or chunking along headings, takes a few lines of the configuration. `nest modules list` shows the modules available, and `nest validate` catches mistakes before anything runs. [Modules](docs/modules.md) (Japanese)
- **Formats it cannot read go through your own converter.** Name a conversion command in `loader.command`, and its output is taken in as the text of formats not read by default, such as drawings or in-house formats.
- **Models can be swapped.** Embedding and answer models come from a local Ollama or an API such as OpenAI's. A reranker connects to any server with `/v1/rerank` or `/rerank` (llama.cpp, vLLM, Infinity, Text Embeddings Inference).
- **Want a new module? Go ahead.** A module is added by defining its role, input, output and arguments in the registry in `src/registry.rs`. Knowledge-Nest is MIT licensed, the steps are in [Contributing](CONTRIBUTING.md), and pull requests are welcome.

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
