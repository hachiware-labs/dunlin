# Guide: change, measure, compare, release

[日本語](guide.md)

"Search seems to miss. Would smaller chunks help?" This guide is about answering that with numbers instead of a hunch.

You cannot tell how good search is by looking. A setting that helps one question may break another. In Knowledge-Nest, once you register pairs of a question and its answer, every change of setting tells you how many of the questions found their answer. And since only what you changed, and what follows it, is rebuilt, trying is fast, so you can compare as many variations as you like.

It assumes `build` and `eval` work, as in [section 3 of Getting started](getting-started.en.md#3-improve-search-by-numbers-new-commands).

> The new commands in this guide support text and Markdown (`.md`, `.markdown`, `.txt`), keyword search, and evidence recall for now. Vector search, user dictionaries, PDF and Office loading, `serve`, and more use the form of configuration that `init` writes (see [9 What still uses the earlier flow](#9-what-still-uses-the-earlier-flow)).

## 1 The idea

```text
documents → extract → transform → index → search → eval
            (docs)    (chunks)    (index)  (query)  (result)
```

- The result of each section is kept as a version in the store (`.knowledge-nest/store`). The same input and the same processing never run twice.
- When you change a setting, **only that section and the ones after it** run. Changing `transform` does not read the documents again.
- A result can have a name. Names live on separate shelves by kind: documents, chunks, indexes, evaluation data, and evaluation results.
- Improving search is a loop: measure a baseline, change one thing, measure again, compare, choose.

## 2 Prepare evaluation data

Evaluation data is a set of questions, each with the quote that answers it. Write a JSON array and bring it into the store with `eval-data import`.

```json
[
  { "id": "pump-check", "question": "ポンプの点検", "source": "docs", "document": "manuals/pump.md",
    "quote": "ポンプの点検は毎月最初の営業日に行う。", "split": "dev" },
  { "id": "pump-seal", "question": "シールの交換", "source": "docs", "document": "manuals/pump.md",
    "quote": "型番 P-200 のシールは、2年ごとに交換する。", "split": "dev" }
]
```

```sh
nest eval-data import cases.json
nest eval-data import cases.json --tag v2
```

- `source` and `document` say where the quote is. `source` is the `id` of `source.filesystem` (the folder name if omitted); `document` is the path inside that folder.
- Use a `quote` that appears exactly once in that document.
- Write questions the way your users ask them. A question copied from the text is easy to answer and tells you little.
- Tune on `split: dev` cases. Measure `split: test` cases once, before you adopt a change.
- Importing the same content again makes no new version. Adding cases makes a new version, and the name (`default` or your `--tag`) moves to it.

> `eval-data generate`, which asks a model to propose questions and quotes, is a command of the earlier flow ([evaluation.md](evaluation.md), Japanese). You can write its candidates out as JSON and import them here.

## 3 Measure a baseline and read the misses

Add an evaluation section to the configuration:

```yaml
eval:
  - eval.evidence: { data: default, split: dev }
```

```sh
nest eval
nest results
```

`eval` builds what is missing, measures, and prints the recall:

```text
Recall: 0.667 (2 of 3 cases)
```

`nest results` lists the named evaluation results, best recall first. Give it a name to see each case and where its evidence was found:

```sh
nest results trial-400
```

```text
Result version 7 (trial-400): recall 0.667 (2 of 3), index main-400 with data default
Found at  Case         Question
missed    filter       フィルターの交換
1         pump-check   ポンプの点検
2         pump-seal    シールの交換
```

- `Found at` is the rank at which the quote appeared in the result. `missed` means it did not appear; those cases are listed first.
- Reproduce a miss with `nest search "<question>"` and look for the cause.

| How it misses | What to try |
|---|---|
| The answer is buried in a long chunk | Change `size` and `overlap` of `chunk.length`. For documents with headings, use `chunk.structure` |
| "Replace every 2 years" does not say what it is about | Chunk context (section 5) |
| The answer is found but ranked low | Change `top_k` of `retrieval.keyword` |

## 4 Change one thing and compare (by hand)

Copy `KnowledgeNestfile.yaml` and rewrite only the sections you want to change. To keep the baseline's names where they are, give the changed sections new tags and start from the baseline's documents with `from`.

```yaml
# candidate.yaml
extract:
  - from: docs

transform:
  - chunk.length: { unit: characters, size: 400, overlap: 50 }
  - tag: chunks-400

index:
  - index.keyword: { tokenizer: lindera }
  - tag: main-400

search:
  - retrieval.keyword: { top_k: 10 }

eval:
  - eval.evidence: { data: default, split: dev }
  - tag: trial-400
```

```sh
nest eval candidate.yaml
nest results
```

- `from: docs` does not scan the folder. It uses the version of documents named `docs`, so both runs are measured on the same documents.
- The result is kept as `trial-400`. `nest results` lists it next to the baseline's result, so you can compare recall.
- A comparison is meaningful only if the evaluation data and method are the same. Keep `data` and `split` of `eval.evidence` identical.

## 5 Give chunks some context

A chunk cut out of a document loses which document and which section it came from. With `context`, Knowledge-Nest puts that information at the start of the text the index searches.

```yaml
transform:
  - chunk.structure:
      max_chars: 800
      context: { document: true, headings: true, prepend: true }
```

| Argument | Meaning |
|---|---|
| `headings` | Record the headings above the chunk in the chunk's information |
| `document` | Include the document's name in the context (use together with `prepend`) |
| `prepend` | Put one line of context at the start of the text the index searches |

- The text a search returns, and its position, do not change. Only the string the index searches does.
- Only Markdown `#` headings are read. Other formats give an empty list.
- Changing `context` rebuilds chunks and the index. Documents are not read again.

## 6 Try many settings at once (optimize)

Instead of writing each candidate by hand, describe what to vary. Knowledge-Nest makes the candidates and measures them one after another.

```yaml
# tune.yaml
optimize:
  base: KnowledgeNestfile.yaml
  vary:
    - name: chunking
      options:
        as-is: {}
        length:
          transform:
            - chunk.length: { unit: characters, size: [200, 400], overlap: 0 }
        structure:
          transform:
            - chunk.structure: { max_chars: 800 }
    - name: search
      options:
        top10: {}
        top3:
          search:
            - retrieval.keyword: { top_k: 3 }
  eval:
    data: default
    split: dev
  limit:
    candidates: 20
```

```sh
nest optimize tune.yaml
```

Progress is printed one line at a time (an example; numbers and names depend on the instruction):

```text
[1/N] 000 base
      recall 0.667
...
```

When it finishes, you get a table, best recall first:

```text
Trial  Recall  To base  Ran                  Time   What
010    1.000   +0.333   transform+index+eval ...    chunking=length(size=200),search=top10
000    0.667   -        nothing              ...    base
```

- An **axis** is something to vary; its **options** are the choices. One option from each axis makes one trial. The example combines four chunking choices (`as-is`, two sizes of `length`, `structure`) with two search choices, and also runs the base (trial 0).
- An array as an argument value (`size: [200, 400]`) expands into options on the spot.
- An empty option (`{}`) means "as in the base". The base is always measured, so there is always something to compare with.
- An option replaces a whole section. Two axes that rewrite the same section are an error, and options cannot rewrite `eval`, `tag`, or `from`.
- Trials share what they have in common. Trials that use the same chunks build those chunks once.
- `limit.candidates` caps the number of candidates.
- A failed trial is recorded with its reason and the next one runs. After three failures in a row, the rest are not run.
- Run the same instruction again and finished trials are reused; only the rest run.
- An optimization never moves the `default` of any kind.

To look at results again:

```sh
nest results            # evaluation results, and the optimizations that were run
nest results tune       # the table of the optimization called tune
nest results tune/010   # the cases of one trial
```

Without `--name`, an optimization is called after the instruction file (without its extension).

Each trial's configuration file is written to `.knowledge-nest/trials/<name>/<number>.yaml`. To rerun one trial, give that file:

```sh
nest eval .knowledge-nest/trials/tune/010.yaml
```

> For now you can vary `transform`, `index`, and `search`. Options that rewrite `extract`, measures other than recall, and choosing the next values from earlier results are not there yet.

## 7 Release

What is served changes only when you say so. `build`, `eval`, and `optimize` never change it. Even after you add documents and rebuild, the previous index stays served until you check the new one and `release` it. A deleted document or a wrong setting never reaches users before you have looked.

```sh
nest build                  # make; what is served does not change
nest list indexes           # see the indexes made, and the one being served
nest search "..." --index main
nest eval                   # check
nest release main           # serve the index you checked
```

| Operation | What is served |
|---|---|
| `nest build`, `nest eval`, `nest optimize` | Unchanged |
| `nest build --release` | Switches to the index just made. A shortcut if you want to serve every build |
| `nest release <index> [--name <name>]` | Switches to the index you name. The default name is `release` |

- **By name or by number.** `release` takes an index's name (`main`, `tune/010`) or the version number shown by `nest list indexes`. For the index listed as `v7`, run `nest release 7`.
- **Go back to an earlier version.** Release that index again. It says which version it switched from and to, and the record is never erased.
- `nest list indexes` shows which index is served under which name.

> `serve` does not read these served names yet. To serve over MCP or HTTP, use the earlier flow.

## 8 Choose which changes of the folder to take in

`apply` of `source.filesystem` chooses which changes of the folder a build takes in.

```yaml
extract:
  - source.filesystem: { path: ./docs, apply: [add, update] }
  - loader.text: {}
  - tag: docs
```

- Choose from `add`, `update`, and `delete`. Without `apply`, all three are taken in.
- Changes that were not taken in are shown by `build` as "Not taken in", and they take effect once you let the setting take them.
- With `[add, update]`, for example, a document removed from the folder stays searchable.

## 9 What still uses the earlier flow

These are not in the new flow yet. Use the configuration that `nest init` writes.

| To do this | Earlier command or page |
|---|---|
| Vector search, hybrid search, reranking | Embedding settings with `index.vector` and `fusion.rrf`. [MCP and RAG](mcp.md) · [Modules](modules.md) |
| User dictionary | `dictionary suggest` and `dictionary add`. [Evaluation and dictionary](evaluation.md) |
| Generate and review questions | `eval-data generate` and `eval-data review`. [Evaluation and dictionary](evaluation.md) |
| PDF, Office, HTML, OCR | `loader.auto` and `nest add ocr`. [PDF extraction](pdf-extraction.md) |
| Serve over MCP or HTTP, `ask` | `serve`, `ask`. [MCP and RAG](mcp.md) · [11 Choose what to serve and whom to let in](#11-choose-what-to-serve-and-whom-to-let-in) |
| Tuning with an agent | [Agents and trials](agents.md) |

Which flow a command uses depends on the form of the configuration file it reads. Sections written as lists of steps such as `- source.filesystem: ...` use the new flow; `modules:` with `use:` uses the earlier flow. One project may hold files of both forms. (Most of the linked pages are in Japanese.)

## 10 Using Wiki and PARA well

Wiki and PARA are a separate mechanism and are not in this guide's versioned store. See [Wiki and PARA](wiki.md) (Japanese). The essentials:

- Ingest with `nest ingress <path>`. A statement whose citation does not match the source word for word is not saved and is reported as a `CITATION_REJECTED` warning.
- `nest verify` checks citations, source versions, and links. `nest verify --content` uses a model to look for contradictions between pages.
- Ask a page to change with `nest instruct <page> "instruction"`. Add a rule for all future work with `nest instruct --persistent "rule"`.
- Changes are recorded as jobs. `nest undo <job-id>` undoes one. Originals are never rewritten.
- Follow a long ingest with `nest progress <job-id>`.
- `serve` offers the pages both on the web and over MCP. What people fix in the browser becomes the knowledge agents use over MCP.

## 11 Choose what to serve and whom to let in

What `nest serve` offers, and whose connections it accepts, is set in the `serve` section of `KnowledgeNestfile.yaml`. `init` writes every default, so change only what you need. These are the defaults for RAG:

```yaml
serve:
  transport: http
  host: 127.0.0.1          # address to listen on; by default only this machine
  port: 4318
  endpoints:               # what to offer; anything not listed is not started
    - responses            # POST /v1/responses: answers with sources
    - search               # POST /v1/search: results and context
    - mcp                  # /mcp: MCP over HTTP for agents
  access:
    allowed_hosts:         # names accepted as the request's Host
      - localhost
      - 127.0.0.1
    allowed_origins: []    # origins whose pages may call from a browser
    auth:
      type: none           # none or token
      token_env: null      # with token, the name of the variable holding it
  limits:
    max_request_bytes: 1048576
    request_timeout_seconds: 120
    max_concurrent: 8
```

**Endpoints** depend on the project type.

| Type | `transport` | `endpoints` you can choose | Default port |
|---|---|---|---|
| `mcp` | `stdio` only | none (writing `host` through `limits` is `ARG_CONFLICT`) | none |
| `rag` | `http` | `responses`, `search`, `mcp` | 4318 |
| `wiki`, `para` | `http` | `web` | 4319 |

- To offer search without the answering API, use `endpoints: [search, mcp]`.
- An endpoint the type does not have is `ARG_RANGE`.

**Whom to let in** (`access`) is checked in this order.

- **Client.** With `allowed_clients`, only the listed address ranges and the machine running the server may connect; anyone else gets `403 CLIENT_FORBIDDEN`. Write an address and the number of leading bits a client shares with it, such as `192.168.11.0/24` for every `192.168.11.*` address, or a single address such as `192.168.11.7`. Left out, clients are not limited by address.
- **Host.** A request whose `Host` is not in `allowed_hosts` gets `403 HOST_FORBIDDEN`. Write a host name or IP address without a port. If people reach the server by an internal name such as `nest.example.internal`, add that name.
- **Origin.** A browser request whose `Origin` is neither the server itself nor in `allowed_origins` gets `403 ORIGIN_FORBIDDEN`. Write it with the scheme and no path, such as `https://app.example.com`. Left empty, pages on other sites cannot call the server, so a page you open cannot send requests to the server on your machine.
- **Token.** When `host` is not a loopback address such as `127.0.0.1`, `auth.type: token` is required, unless `allowed_clients` lists only the ranges of home and office networks (`10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`, link-local addresses and `fc00::/7`); then the server also starts with `auth.type: none`. Keep the token out of the configuration file: `token_env` names the environment variable that holds it. Clients send `Authorization: Bearer <token>`. The Wiki and PARA pages also accept Basic authentication, with the user name `knowledge-nest` and the token as the password.

An example that opens the server to other machines:

```yaml
serve:
  host: 0.0.0.0
  access:
    allowed_hosts: [nest.example.internal]
    allowed_origins: [https://app.example.com]
    auth: { type: token, token_env: NEST_TOKEN }
```

An example that opens the server to a home or office network only. Add this machine's address on that network to `allowed_hosts`; other devices open it by that address, such as `http://192.168.11.16:4319/`.

```yaml
serve:
  host: 0.0.0.0
  access:
    allowed_hosts: [localhost, 127.0.0.1, 192.168.11.16]
    allowed_clients: [192.168.11.0/24]
```

This example has no token: every device in the range can read and write. On the Wiki and PARA pages, it can also take in a file of the server's machine by its path. If the network has devices you do not trust, add `auth: { type: token, token_env: NEST_TOKEN }`.

**Setting it with a command.** Instead of editing the file, `nest config` writes the access settings.

```sh
# For every project on this machine (kept in ~/.knowledge-nest/config.yaml)
nest config set serve --host 0.0.0.0 --allowed-hosts localhost,127.0.0.1,192.168.11.16 --allowed-clients 192.168.11.0/24

# For one project only (rewrites that project's KnowledgeNestfile.yaml)
nest config set serve --allowed-clients 192.168.11.0/24 --project ./my-wiki

nest config list                              # the settings of this machine
nest config list --project ./my-wiki          # the settings of the project
nest config unset serve                       # remove the settings of this machine
nest config unset serve --project ./my-wiki   # put the standard values back in the project
```

- It sets `host`, `port`, `allowed_hosts`, `allowed_clients` and `auth`. A port is written as `--port 8080`. A port in the settings of the machine goes to every project that still has the standard port of its type (4318 or 4319), so when several projects run at once, choose a port for each with `--project`. Authentication is `--auth none`, or `--auth token --token-env NEST_TOKEN`. Lists are separated by commas. An empty value (`--allowed-clients ""`) removes that setting.
- **The project wins.** Where the project file differs from the standard value that `init` wrote, the project's value is used; where it still has the standard value, the machine's value is used. `nest serve --host` and `--port` win over both.
- The token itself is never stored. `--token-env` takes the name of an environment variable; put the value in that variable.
- A `host` outside loopback is refused unless `--allowed-clients` with home or office ranges, or `--auth token`, comes with it.

```sh
export NEST_TOKEN=...      # use a long random value
nest validate --json       # reports conflicts before you start the server
nest serve
```

- `nest serve --host` and `--port` change the values for that run only. The checks above apply to the values after the change.
- `serve` settings are not part of the build ID or search configuration ID. Changing the port or access needs no rebuild or new evaluation.
- There is no encryption and no limit by client address range. To serve inside an organization, leave those to a reverse proxy in front.

`serve` decides only which connections come in. Where models are reached is `base_url` in `models` (a local Ollama by default). There is no setting that allows or limits the web addresses to take documents from.

[HTTP limits](mcp.md#http-の接続制限) · [Spec of the serve settings](knowledge-nest-spec.md#275-serve-の設定項目) · [Full configuration files](config.md) (Japanese)

## 12 Checklist

- [ ] At least 10 evaluation cases, phrased like real questions
- [ ] The baseline is measured with `nest eval` and its result has a name
- [ ] Trials use the same evaluation method and data as the baseline
- [ ] You looked at how the `missed` cases changed, not only the average
- [ ] You measured with `split: test` before adopting a change
- [ ] You checked the released index with `nest list indexes`
