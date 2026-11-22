# Building, looking, searching, releasing (staged projects)

## The commands

```sh
dunlin build [<config>] [--release]     # extract, transform, index
dunlin eval [<config>]                  # evaluate; builds first what is missing
dunlin list [documents|chunks|indexes|eval-data|results]
dunlin status                           # the same as dunlin list
dunlin validate [--config <file>]       # check a configuration without building
dunlin search "<question>" [--index <name or version>] [--limit <n>]
dunlin release <index name or version> [--name <served name>]
dunlin results [<name>]
```

Without `<config>`, `build` and `eval` read `Dunlinfile.yaml` in the project folder. Run
the commands from the project folder, or add `--project <folder>`.

## Build

```sh
dunlin build
```

```text
Ran extract, transform, index.
Documents: 1 added, 1 updated, 1 deleted, 4 unchanged.
  + new.md
  ~ guide.md
  - faq.txt
Chunked 2 documents.
Index: version 8 (main, default)
```

Read this output; it is the answer to "what changed".

- `Documents:` compares the folder with the last build. Only added and updated documents
  are read and chunked again.
- `Not taken in, as apply says:` lists changes the configuration told Dunlin to leave out.
- `No loader takes N files:` lists files that were skipped. Tell the user: those documents
  are not searchable.
- `Nothing ran: every result was there already.` means the folder and the settings are the
  same as in an earlier build. That is success, not a failure.

Building again is always safe. It never changes what is served.

**Compare what the build reports with what the user said they did.** A user who "added and
fixed some documents" and a build that also reports a deletion do not match: name the
deleted document and ask whether it was meant, before anything is released. A deleted
document disappears from the search.

## Look at what exists

```sh
dunlin list
```

```text
Documents  docs, default        v6   2026-11-22 05:34Z  6 documents
Chunks     chunks-200, default  v7   2026-11-22 05:34Z  6 documents chunked, from documents docs, default
Indexes    main, default        v8   2026-11-22 05:34Z  from chunks chunks-200, default
           -                    v4   2026-11-22 05:34Z  from chunks version 3  [served as release]
```

- Each row is one version, with all its names and its version number (`v8`).
- `[served as release]` marks the index that users of the search get now.
- A row with `-` has no name left; it is listed because it is served.
- Times are UTC.

In the example the served index (v4) is older than the latest (v8): documents were rebuilt
and not released. That is the normal state between a build and a decision to release.

## Search to try an index

```sh
dunlin search "バルブの締め付け"                 # the latest index (default)
dunlin search "バルブの締め付け" --index main
dunlin search "バルブの締め付け" --index 4       # a version number
dunlin search "バルブの締め付け" --index release # what is served under the name release
```

Searching the latest and the served index with the same question is the quickest way to
show a user what a release would change.

## Release and roll back

```sh
dunlin release main        # serve the index named main
dunlin release 4           # serve version 4 again: this is the rollback
```

```text
Served as release: version 8 (main, default). It was version 4.
```

The message names the version that was served before. Keep it in your report: it is what
`dunlin release <that version>` needs to undo the release.

`dunlin build --release` builds and releases in one step. Use it only when the user wants
every build to go live. Otherwise build, evaluate, then release.

Release changes what real users get. Do it when the user asks for it.

## After documents change: the usual sequence

```sh
dunlin build                       # take in the changes; read the Documents: lines
dunlin eval                        # same evaluation data as before
dunlin results default             # which questions are missed now
dunlin search "<a question about the new document>"
dunlin release <index>             # when the user agrees
```

If recall dropped after documents were added, look at the missed cases first. Common
causes: a new document competes for the same words, or a deleted document held the
evidence of a case (then the evaluation data needs updating, not the settings).

## What is not there yet

Say so plainly if the user asks for one of these in a staged project:

- A preview of what a build would change without building (`--dry-run`).
- Removing old versions or trials to free space.
- The history of a name (which versions it pointed at).
- `serve` and `ask` for a staged project. To serve over MCP or HTTP today, the project has
  to be a classic one (`references/classic.md`).
