# Getting started: turn your documents into knowledge AI can use

[日本語](getting-started.md)

This is the entrance for first-time users. Pick what you want to do and go to its introduction. Each one starts from a single folder of documents, with no programming.

## 1 Start from what you want to do

| What you want | AI model | Introduction |
|---|---|---|
| **Let Claude or another agent look things up in my documents** | Not needed | [MCP](start-mcp.en.md) |
| **Have my app or chatbot return answers with sources** | Needed to answer | [RAG](start-rag.en.md) |
| **Write up my documents as knowledge pages my team uses** | Needed to write | [Wiki](start-wiki.en.md) |
| **Organize the documents and notes of my own work by my projects** | Needed to write | [PARA](start-para.en.md) |

- **If unsure, start with the first.** No model to prepare; you can try it in minutes, and make the other forms from the same documents later.
- **Wiki and PARA both organize documents, for different ends.** The Wiki builds knowledge everyone uses, by what it is about; PARA organizes the documents of your own work, by which project they serve.
- **Always with sources.** Search results, answers and pages all point back to the original documents.

How Knowledge-Nest differs from other tools, and how the four forms differ, is in [How the four forms differ](../README.en.md#how-the-four-forms-differ) in the README.

## 2 What you need

- Knowledge-Nest installed. For now it is built from source; see [Install in the README](../README.en.md#install). You are ready when `nest --help` prints.
- A folder of documents. If you have none at hand, use [examples/manuals](../examples/manuals/maintenance.md) from the repository: a short Markdown file on pump maintenance (in Japanese).
- The second to fourth need an AI model. The introductions use `qwen3:4b` on a local Ollama.

To try all four at once on the sample, there is also [Start with the sample](quickstart.md) (Japanese).

## 3 After the introduction

| What you want | Read |
|---|---|
| Measure how well search finds answers, and improve it | [Improve search by numbers](guide.en.md) · [Evaluation and dictionary](evaluation.md) (Japanese) |
| Use it from other machines, and limit who connects | [Choose what to serve and whom to let in](serve.en.md) |
| Learn the Wiki and PARA screen, ingest and checks in detail | [Using Wiki and PARA](wiki.md) (Japanese) |
| Look up commands and settings | [CLI reference](cli.md) · [KnowledgeNestfile defaults](config.md) (Japanese) |
| Something goes wrong | [Troubleshooting](troubleshooting.md) (Japanese) |
