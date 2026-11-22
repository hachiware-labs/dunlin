# Classic projects

A classic project is what `knowledge-nest init` creates. Its `KnowledgeNestfile.yaml` has `source:`,
`transform:` and `index:` sections that hold `modules:` with `- use: <module>` entries.
It supports everything Knowledge-Nest ships: PDF and Office documents, OCR, vector and hybrid
search, answering with a model, serving over MCP and HTTP, and the wiki.

The commands and options of this style are many and change between versions. This file
gives the shape of the work; take the exact arguments from `knowledge-nest <command> --help` and
the modules from `knowledge-nest modules list` and `knowledge-nest modules describe <name>`.

## The four project types

| Type | For | Created with |
| --- | --- | --- |
| `mcp` | Search over documents, offered to an MCP client | `knowledge-nest init mcp <folder>` |
| `rag` | Answers with sources, over HTTP | `knowledge-nest init rag <folder>` |
| `wiki` | Pages written from sources by a model | `knowledge-nest init wiki <folder> --purpose "<what the wiki is for>"` |
| `para` | Notes sorted into Projects, Areas, Resources, Archives | `knowledge-nest init para <folder> --purpose "<...>"` |

`init` takes the models and tools to use (`--llm-provider`, `--llm-model`,
`--embedding-provider`, `--no-embedding`, `--ocr`, `--no-ocr`, ...). Ask the user which
models they have before choosing; `--no-embedding --no-ocr` gives a keyword-only project
that needs no model at all, which is the safe start.

## Search projects (mcp, rag)

```sh
knowledge-nest build                       # read the documents, build the index
knowledge-nest search "<question>"         # passages with their sources
knowledge-nest ask "<question>"            # rag: an answer with sources (needs an answering model)
knowledge-nest status                      # what is served, its evaluation, storage
knowledge-nest serve                       # serve over MCP or HTTP
knowledge-nest serve --print-config        # the entry to paste into an MCP client's settings
```

`knowledge-nest validate` checks the configuration without building. `knowledge-nest build --dry-run`
shows what a build would do.

`knowledge-nest serve` keeps running until stopped. Start it only when the user asks for the server,
and tell them how to stop it. To check that serving works, prefer `knowledge-nest search` and
`knowledge-nest serve --print-config`.

## Evaluate and try a change

Evaluation data is managed with `knowledge-nest eval-data` (`add`, `generate`, `review`, `import`,
`export`, `list`, ...). A case is a question and a quote that appears exactly once in a
document.

```sh
knowledge-nest eval-data add --question "<question>" --doc "<source>:<file>" --quote "<exact text>"
knowledge-nest eval --tag baseline             # measure what is served; name the report
knowledge-nest report show baseline
knowledge-nest tag baseline-build              # name the build that is served now
```

To try a change, copy the configuration, edit the copy, and build a trial from the stage
that changed, on top of the baseline build:

```sh
cp KnowledgeNestfile.yaml candidate.yaml      # then edit candidate.yaml
knowledge-nest validate --config candidate.yaml
knowledge-nest build --from transform --input baseline-build --config candidate.yaml --tag trial
knowledge-nest eval --build trial --tag trial-eval
knowledge-nest report compare baseline trial-eval
```

- `--from` is the first stage to run again: `transform` for chunking, `index` for the
  index or a dictionary. A change to search settings alone needs no build: pass the
  candidate to `knowledge-nest search --build baseline-build --config candidate.yaml` and to
  `knowledge-nest eval --build baseline-build --config candidate.yaml --tag trial-eval`.
- In the comparison, check that `comparable` is true before reading the scores, then read
  which cases were recovered and which were lost.
- `knowledge-nest eval-data generate` asks a model for candidate cases. They are not used until
  reviewed. It sends document text to that model's provider: say so before running it.
- `knowledge-nest dictionary suggest --report <tag>` proposes user dictionary words when a
  question's word is split differently from the document's.

Adopting a trial is `knowledge-nest use <build>`. It changes what is served: leave it to the user
unless they ask you to do it.

## Wiki and PARA

```sh
knowledge-nest ingress <folder or file>    # read sources and write pages from them
knowledge-nest query "<question>"          # answer from the pages
knowledge-nest verify                      # check pages against their sources
knowledge-nest serve                       # browse the pages
```

`ingress`, `instruct`, `archive` and `undo` change pages. `knowledge-nest progress <work-id>` shows
a running job and `knowledge-nest undo <work-id>` reverts one. Take the rest from `--help`.

## Reading results

With `--json`, a command prints one object: `status` is `succeeded` or `failed`; `data`
holds the result; `errors[]` has a `code` and a `message`. Report the code and message of
a failure as they are. `run_id` identifies a run when something has to be traced.

## Moving a classic project to the staged style

There is no converter. The staged style supports fewer modules (text and Markdown,
keyword search only), so a project that uses PDFs, vectors or a model cannot move yet.
Do not rewrite a working classic `KnowledgeNestfile.yaml` into the staged form unless the user
asks for it and the project only uses what `references/staged-config.md` lists.
