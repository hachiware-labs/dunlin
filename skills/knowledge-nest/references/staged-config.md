# The staged configuration file

A staged `KnowledgeNestfile.yaml` says what each stage does, as a list of modules that run in
order. Any file name works: the file is given to `knowledge-nest build <file>` or
`knowledge-nest eval <file>`; without one they read `KnowledgeNestfile.yaml` of the project.

## A complete file

```yaml
extract:
  - source.filesystem: { path: ./docs }
  - loader.text: {}
  - tag: docs

transform:
  - chunk.length: { unit: characters, size: 400, overlap: 40 }
  - tag: chunks-400

index:
  - index.keyword: { tokenizer: lindera }
  - tag: main

search:
  - retrieval.keyword: { top_k: 10 }

eval:
  - eval.evidence: { data: default, split: dev }

serve:
  tag: release
```

Paths are relative to the project folder, not to the configuration file. A trial file in
`.knowledge-nest/trials/...` still means the project's `./docs`.

## Sections

| Section | Makes | Needed |
| --- | --- | --- |
| `extract` | the documents | yes, or `- from: <name>` |
| `transform` | the chunks | yes, or `- from: <name>` |
| `index` | the index | yes, or `- from: <name>` |
| `search` | nothing; how the index is searched | yes |
| `eval` | an evaluation result | only for `knowledge-nest eval` |
| `serve` | nothing; the name an index is served under | only for `--release` |

Two entries are not modules:

- `- tag: <name>` as the **last** entry names what the section makes.
- `- from: <name>` as the **only** entry starts from something that exists already, instead
  of making it. Use it to try another chunking on the same documents without scanning the
  folder again:

  ```yaml
  extract:
    - from: docs
  transform:
    - chunk.structure: { max_chars: 300 }
    - tag: chunks-structure
  ```

## Modules

The staged style supports these. Anything else is refused with "is not on the new store
yet": do not write it.

| Module | Section | Arguments |
| --- | --- | --- |
| `source.filesystem` | extract | `path` (folder, required). `id` (name of the source; default is the folder's name). `apply` (see below) |
| `loader.text` | extract | none. Reads text and Markdown files |
| `text.normalize` | transform | `unicode: nfkc` only |
| `chunk.length` | transform | `size` (characters, required), `overlap` (smaller than `size`), `unit: characters` only, `context` |
| `chunk.structure` | transform | `max_chars` (required), `context`. Splits at headings and paragraphs |
| `index.keyword` | index | `tokenizer: lindera` only |
| `retrieval.keyword` | search | `top_k` (1 to 100, required) |
| `eval.evidence` | eval | `data` (name of evaluation data, required), `split` (`dev` or `test`; default `dev`) |

`transform` takes exactly one chunking module. Vector indexes, hybrid search, rerankers,
PDF and Office loaders and LLM modules are not supported in the staged style yet.

### `apply`: which changes of the folder are taken in

```yaml
- source.filesystem: { path: ./docs, apply: [add, update] }
```

A list of `add`, `update`, `delete`. The default is all three. Leaving one out means that
kind of change is not taken in: with `[add, update]` a file deleted from the folder stays
in the documents. `knowledge-nest build` prints what was left out.

### `context`: a chunk knows where it is

```yaml
- chunk.structure:
    max_chars: 400
    context: { headings: true, document: true, prepend: true }
```

- `headings: true` records the headings above each chunk.
- `prepend: true` makes the document name and those headings part of what is searched, so
  a chunk under "Pump > Inspection" is found by "pump inspection" even when its own text
  says neither word. The text returned to the user is unchanged.
- `document: true` needs `prepend: true`. `prepend: true` needs `headings` or `document`.

This helps most with documents that have a clear heading structure and short sections.

## Names

Every version Knowledge-Nest makes can have names, and each kind of thing has its own names:
documents, chunks, indexes, evaluation data and results. The same name in two kinds does
not clash.

- **`default`** is given automatically to whatever a `build` or `eval` run by hand made
  last, for each kind. With no `tag` anywhere, everything is still reachable as `default`.
- **A `tag`** is a name you choose. It moves to the new version each time the section runs.
- **The served name** (`serve: { tag: release }`) is separate from tags. Only a release
  moves it.

Write tags when two configurations live side by side, so that each keeps its own names.
For a single configuration, `default` is enough.

## Checking a file

```sh
knowledge-nest validate                      # KnowledgeNestfile.yaml of the project
knowledge-nest validate --config other.yaml
```

```text
KnowledgeNestfile.yaml can run: extract, transform, index, eval.
```

`validate` builds nothing. `knowledge-nest build` makes the same check before it does anything, so
a file that is wrong never half-runs. The messages are specific ("chunk.length needs
overlap smaller than size"); fix what they name.
