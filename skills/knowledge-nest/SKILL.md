---
name: knowledge-nest
description: Build, evaluate, tune and serve document search (RAG, MCP, wiki) with the Knowledge-Nest CLI, whose command is `nest`. Use this skill whenever the user works in a project that has a KnowledgeNestfile.yaml or a .knowledge-nest folder, mentions Knowledge-Nest, runs or asks about a `nest` command (nest build, nest eval, nest optimize and the like), or wants to index a folder of documents for search, measure retrieval quality with evaluation data, compare chunking or search settings, run an optimization, see which questions a search misses, release or roll back an index, or get an HTML report of evaluation results. Use it also for writing or fixing Knowledge-Nest configuration files and optimization instructions, even when the user only says "improve the search" or "why is this question not found".
---

# Knowledge-Nest

Knowledge-Nest is a command line tool that turns a folder of documents into something that can be
searched: it extracts the documents, chunks them, builds an index, serves it (MCP, HTTP or a
wiki), and measures how well a search finds the evidence for a set of questions.

Your job with this skill is to drive that tool well: write its configuration, run it in the
right order, read what it reports, and tell the user what was found and what you recommend.

## Start by looking

Knowledge-Nest changes between versions and a project can be written in one of two styles, so look
before you act. These three calls cost almost nothing and prevent most mistakes.

```sh
nest --help                 # the commands this version has
nest <command> --help       # the arguments of one command
nest modules list           # the modules a configuration can name
```

A command or option that `--help` does not show does not exist in the installed version.
Do not use it and do not guess a replacement: say that the installed version lacks it.

`nest build --help`, `nest eval-data --help` and `nest optimize --help` also show the
file formats the installed version takes. Where they differ from this skill, they are right.

Then read the project's `KnowledgeNestfile.yaml` and decide which style it is.

| The file looks like | Style | Read next |
| --- | --- | --- |
| `extract:`, `transform:` and `index:` are **lists** of `- module.name: { args }` | staged | `references/staged-config.md` |
| `source:`, `transform:`, `index:` each hold `modules:` with `- use: module.name` | classic | `references/classic.md` |
| There is no `KnowledgeNestfile.yaml` | none yet | ask what the user wants to build, then `references/classic.md` for `nest init`, or `references/staged-config.md` to write a staged file by hand |

The style matters because `build`, `eval`, `search` and `eval-data` exist in both and behave
differently: Knowledge-Nest picks the behaviour from the configuration file it reads. `list`,
`release`, `optimize` and `results` work only with a staged project.

Add `--json` to any command when you need to read its result exactly. The result is one
JSON object with `status`, `data` and `errors`; progress goes to standard error.

## What to read for each task

| The user wants to | Read |
| --- | --- |
| Create a project, serve it, ask questions, keep a wiki | `references/classic.md` |
| Write or change a staged configuration file | `references/staged-config.md` |
| Build, rebuild after documents changed, look at what exists, search, release, roll back | `references/workflow.md` |
| Create evaluation data, measure a build, see the missed questions | `references/evaluation.md` |
| Compare settings, write an optimization instruction, pick a winner | `references/optimize.md` |
| An HTML report of results or of an optimization | `references/report.md` |

Read only the file the task needs.

## How to work

**Measure before changing.** A change to chunking or search is only an improvement if
recall on the evaluation data says so. If there is no evaluation data yet, making some is
the first step, not an optional one. Without it you can only report impressions.

**Keep the comparison fair.** Trials are compared on the same documents and the same
evaluation data. Do not edit the evaluation data to make a number go up, and do not add or
remove documents in the middle of a comparison. Knowledge-Nest helps here: an optimization starts
every trial from one fixed version of the documents and says which.

**Tune on `dev`, confirm on `test`.** Evaluation cases carry a split. Use `dev` while
trying things. Run `test` once, at the end, on the setting you chose. A setting picked on
`dev` looks better on `dev` than it is.

**Building is safe; serving is the user's decision.** Building, evaluating and optimizing
only add versions to the project's store: nothing is overwritten and what is served does
not move. `nest release`, `nest build --release` and (classic) `nest release` change
what real users of the search get. Recommend, show the numbers, and release only when the
user says so.

**Say what you saw.** Report the command you ran and the numbers it printed. When
something did not run or failed, say so with the message. Do not round a failed trial into
"slightly worse".

## Reporting to the user

Answer in the user's language. After a piece of work, the user needs:

1. What you ran and on what (which configuration file, which documents version, which
   evaluation data and split).
2. The result: recall, how many cases, and which questions were missed.
3. What changed compared to before, when there is a before.
4. Your recommendation, and the one command that would apply it.

For more than a handful of results, make the HTML report (`references/report.md`) and give
the user the file, with the two or three findings that matter in your message.

## When something goes wrong

- **A message says something is "not on the new store yet".** The staged style supports
  fewer modules than the classic one. Use what `references/staged-config.md` lists, or tell
  the user the feature needs a classic project for now.
- **`--<option> does not work with a configuration file of this form yet`.** That option
  belongs to the classic style. The staged way to do the same thing is in
  `references/workflow.md`.
- **Recall is 0 or far lower than expected.** Before blaming the settings, check the
  evaluation data: `source` and `document` of each case must match what `nest build`
  printed, and `quote` must be text that appears in the document exactly.
- **The numbers did not change after documents were added.** An optimization uses the
  documents as they were at the last build. Run `nest build` first.
- **`The index would be made at ... Windows takes at most ...`.** The project sits in a
  folder whose path is too long for Windows. Nothing is wrong with the configuration: the
  project has to move to a shorter path. Tell the user; do not work around it.
- **You are about to probe by trial and error.** Stop after two or three failed attempts
  at the same thing and read the `--help` of the command or the reference file for the
  task. The formats are written down; guessing them costs more than reading.
