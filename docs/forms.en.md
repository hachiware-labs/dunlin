# How the four forms differ

[日本語](forms.md)

This page is for those who want the details of how the four document databases Knowledge-Nest makes (MCP, RAG, Wiki and PARA) differ. To choose from what you want to do, the table in [Getting started](getting-started.en.md) is enough.

## At a glance

| | MCP | RAG | Wiki | PARA |
|---|---|---|---|---|
| What you get | Search for agents | An API that answers with sources | Pages of knowledge with sources | Notes in PARA form |
| Who uses it | Agents | Your app | A team, and agents | You, and agents |
| Why organize | — | — | To build knowledge everyone uses | To get your own work done |
| Arranged by | — | — | What the knowledge is about (subject) | Which project it serves (Projects, Areas, Resources, Archives) |
| When documents are read | At each question | At each question | At ingest | At ingest |
| What an agent receives | Passages of the documents | Passages of the documents (`/mcp`) | Pages the AI wrote | Notes the AI organized |
| AI model | Not needed | Needed to answer | Needed to write | Needed to write |
| Updating documents | `build`, changed documents only | `build`, changed documents only | `ingest` adds to pages | `ingest` adds notes |
| Served as | MCP | HTTP (answers, search) and MCP | Web (pages and a work screen) and MCP | Web (notes and a work screen) and MCP |

MCP and RAG search the documents at each question, so even many or often-changing documents are reflected at once. Wiki and PARA have an AI read and organize the documents first; the result is knowledge people can read as it is.

## How MCP and RAG differ

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

Japanese is segmented with Lindera and IPADIC, and documents that match only on light words such as "する" or "こと" are not returned. Add technical terms with a user dictionary. Hybrid search with vectors, PDF, Office, HTML and OCR are supported too. [MCP and RAG](mcp.md) (Japanese)

## What an LLM-Wiki is

LLM-Wiki is an approach to building a knowledge base with an LLM, proposed by Andrej Karpathy ([the original gist](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f)).

RAG searches the documents for fragments at each question and builds an answer on the spot; nothing it read is kept. In an LLM-Wiki, the LLM writes what it read as linked Wiki pages, and extends the pages a new document concerns. Knowledge becomes something written once and grown. People choose the documents and ask the questions.

| LLM-Wiki operation | What it does | Knowledge-Nest |
|---|---|---|
| Ingest | Read a document and write or revise the pages it concerns | `nest ingest` |
| Query | Ask the Wiki's pages, not the original documents | `nest query` |
| Lint | Find orphan pages, broken links, contradictions and stale statements | `nest lint` (sources, versions, numbers and quotations, links), `--content` (contradictions between pages) |

Text an LLM writes can contain plausible errors. Knowledge-Nest covers this weakness by checking against sources. PARA shares with the Wiki the checks against sources, originals that are never changed, undoable jobs, and serving on the web and over MCP. [Using Wiki and PARA](wiki.md) (Japanese)
