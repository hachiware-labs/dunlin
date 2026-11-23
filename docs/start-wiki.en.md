# Wiki: let an AI write pages of knowledge, with evidence

[日本語](start-wiki.md) · [All introductions](getting-started.en.md)

Have an AI read your documents and write Wiki pages organized by topic. Each page lists the documents it was written from as its sources. People read the pages in a browser, and agents use the same pages over MCP.

## The LLM-Wiki idea

This follows the LLM-Wiki idea proposed by Andrej Karpathy ([the original gist](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f)).

- RAG searches for fragments of your documents at each question and assembles an answer on the spot. Nothing it read is kept.
- An LLM-Wiki keeps what the AI has read, written up as pages. When a new document arrives, the related pages are extended, and the knowledge grows.
- People choose the sources, ask questions, and give instructions. Summarizing, linking pages, and keeping them current are the AI's job.

Knowledge-Nest adds source checks to this. Each page keeps the documents it was written from as its sources, and a program checks that the numbers, dates and quotations of a page appear as written in those documents.

## When to use it

- You want a knowledge base that newcomers can read, built from scattered procedures and minutes
- You want the knowledge people read and the knowledge agents use to be one and the same
- You want to find contradictions between rules and manuals, or statements that went stale

## Who does what

| Role | Who |
|---|---|
| Choose and hand over sources, ask, instruct | People |
| Read sources, write and rewrite pages | The AI (the model you set) |
| Record and check sources, keep originals and versions, record jobs | Knowledge-Nest |

People do not edit pages directly. To change a page, you instruct the AI to change it. That is how every page keeps its sources.

## Compared with other ways

Have a chat summarize your documents, and the answer is gone when the chat ends. Have a notes app's AI write, and you no longer know what came from the documents. Knowledge-Nest's Wiki piles up what it read as pages with sources, and follows the documents when they change.

- **Knowledge piles up.** A new document extends the pages it concerns. A document with nothing new is recorded as having nothing to add, and does not pad the Wiki. An answer worth keeping becomes a page with `nest save-answer`.
- **When a document is revised, only its pages are rewritten.** Ingest a new version, and the AI rewrites just the pages written from that document. `nest lint` points out pages not yet brought up to date.
- **A program doubts what the AI wrote.** At every ingest, the numbers, dates and quotations of a page are checked against its sources. `nest lint --content` names contradictions between pages, terms with no page, and even questions to look into next and documents to look for.
- **What people know is kept as a source too.** Field knowledge that is in no document, such as "we actually replace it every three months", is recorded with who said it, and the page is fixed with it as a source. Each change the AI makes is recorded as a job, and `nest undo` takes it back. Originals are never changed.
- **Knowledge stays as files on your machine.** Pages are HTML files in `wiki/`, with a contents page (`index.html`) and a log of the work (`log.html`). They open without a server and move with the folder. People use the pages in a browser, and agents the same pages over MCP.
- **Collect from pages behind a sign-in, and from X.** Sign in once in nest's own browser, and internal pages can be ingested. X bookmarks come in one post at a time, with the pages they link to and the posts they quote.

## 1 Prepare a writing model

This page uses `qwen3:4b` on a local Ollama.

```sh
nest config set llm --provider ollama --model qwen3:4b
```

## 2 Create the Wiki

```sh
nest init wiki ./wiki --purpose "Collect maintenance procedures and part information"
cd wiki
```

In `--purpose`, write in a line or two what this Wiki is for. It becomes the description on the top page, and it is also what the AI uses to decide what becomes a page and what is out of scope. You can change it later in `about.purpose` and `about.out_of_scope` of `KnowledgeNestfile.yaml`.

## 3 Ingest documents

Put the documents in `raw/` in the Wiki's folder and ingest them from there. Files and folders from outside `raw/` are copied into it first and read from there. The AI only reads what is in `raw/` and never changes it. The pages it writes are in `wiki/`, and the rules for writing them are in `rules.md`.

```sh
cp ../manuals/*.md raw/
nest ingest raw/
```

When you add documents to `raw/`, ingest the new files one at a time with `nest ingest raw/<file>` (given a folder, the AI reads at most 16,000 characters in one run).

`ingress` proceeds in this order:

1. Saves each document as an original, with a version
2. The AI reads it, revises or adds to related pages, and creates new pages where needed. A document that adds nothing to the Wiki gets no pages and is recorded as such ("得るものなし")
3. Adds the documents used to the sources of each page, and warns of numbers, dates and quotations that its sources do not have
4. Returns the list of changed pages

This can take a few minutes. Give the job ID it prints to `nest progress <job-id>` to follow it. To save the originals now and read and write later, add `--source-only`, then run `nest ingress --pending` afterwards.

Web pages can be ingested by URL too. HTML, PDF, Markdown, and Office documents are downloaded, and their content at that moment is saved as the original in `raw/<host>/<path>`. Ingesting the same URL again saves a new version of the same document. Web pages are opened in a browser profile of nest's own. Run `nest browser` once first, sign in to the sites to read in the browser it opens, then close it. Without that profile, ingesting a URL stops with `BROWSER_NOT_READY` ([details](wiki.md)).

```sh
nest ingress https://example.com/manuals/pump
```

## 4 Read: in a browser and over MCP

```sh
nest serve
```

`serve` offers the pages both on the web and over MCP.

**In a browser**: open `http://127.0.0.1:4319`.

| Column | Contents |
|---|---|
| Left | Where things are. **資料を取り込む** (add documents, by URL or file), the home, the job history, the rules, and a file tree. **ページ** (pages) are what the AI wrote; **原本** (originals) are the documents you ingested |
| Right | What you opened, and what you can do with it. Under a page and its sources you can ask about it, have it revised by instruction, and check it. An original shows its ingested text and the pages written from it |

The home shows the purpose and scope, a box for questions, the number of pages, originals, originals not yet reflected, and open findings, the pages changed lately, and the latest jobs. You know what to do next as soon as you open it. The screen itself is in Japanese.

**From an agent**: register it in your MCP client's settings, and Claude or another agent answers from the same pages people read. The pages are also saved as HTML in `wiki/`, readable without a server; `wiki/index.html` lists them all.

## 5 Ask

```sh
nest query "When is the pump inspected?"
```

`query` searches the Wiki pages, not the original documents, and answers with sources. A source leads to the page used for the answer and on to the documents that page was written from. A question outside the purpose is answered as out of scope. Save a good answer as a page with `nest save-answer`. The more you ask, the more the Wiki grows.

## 6 Fix: instructions and rules

```sh
nest instruct <page> "List the checks before inspection as bullets"
nest instruct --persistent "Write procedures as numbered steps"
```

| Kind of instruction | Example | How it is handled |
|---|---|---|
| Change structure or wording | Make it shorter, make it a table | Done as asked. No new facts are added |
| Fix based on the documents | This statement differs from the document | The source is read again and the text rewritten |
| Add something not in the documents | We actually replace it every three months | Recorded as a supplement with `--supplement` and `--by`, then cited |
| A rule for the future | From now on, number the steps | Added to the working rules with `--persistent`; applies to all later work |

List the rules with `nest rules`. On the screen, open a page and write in the right-hand box to do the same.

The AI gives each page a category when it ingests, and adds a new one under `## 分類` (categories) of `rules.md` when none fits. Adding is all it does. When similar categories pile up, edit `rules.md` yourself to merge or rename them. `nest lint` points out pages whose category is no longer listed; move them with `nest instruct <page> --category <category>`. `nest lint --content` names pages whose category does not fit. [Details](wiki.md#分類)

## 7 Check

```sh
nest verify
nest verify --content
```

| Check | What it looks at | Model |
|---|---|---|
| `verify` | Whether the source documents exist and are current, whether the numbers, dates and quotations of a page are in its sources, broken links, pages nothing links to | Not needed. Fast, and the same result every time |
| `verify --content` | Contradictions between pages, conflicts between new documents and existing pages, duplicates | Needed. Findings are candidates and may be wrong |

Findings are shown, not fixed. People decide which side is right and fix it with an instruction. Mark a finding you have handled with `nest issue <id> resolved` (or `dismissed`), and it stops appearing.

## 8 Undo and put away

- **Undo**: every ingest and every instruction is recorded as one job. If you don't like the result, `nest undo <job-id>` takes it back. Originals are never rewritten.
- **Put away**: archive a stale page with `nest archive <page>`. It leaves the table of contents and ordinary questions, but the page stays and links do not break. `--restore` brings it back.

## 9 When something goes wrong

| Symptom | What to do |
|---|---|
| `MODEL_REQUIRED` | Set `models.writer.model`, or pass `--llm-model` to `init` |
| `UNSUPPORTED_LITERAL` warning | Some numbers, dates or quotations of a page were not found in its sources. The page was saved: open it, check them, and have it fixed by instruction if they are wrong |
| `BUILD_BUSY`, `DB_BUSY` | Wait for the previous job to finish. Follow it with `progress` |

Other cases are in [Troubleshooting](troubleshooting.md) (Japanese).

## Where next

- Organize notes by how you act on them: [PARA](start-para.en.md)
- Screen operations in detail: [Wiki and PARA](wiki.md) (Japanese)
- Let other machines open the screen: [Choose what to serve and whom to let in](serve.en.md)
