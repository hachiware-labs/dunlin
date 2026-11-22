# Comparing settings with an optimization (staged projects)

An optimization takes a base configuration and a list of things to vary, writes one
configuration file per combination, builds and evaluates each, and shows them side by side.
Use it instead of editing the configuration by hand and re-running: it keeps every trial,
reuses whatever two trials share, and measures all of them the same way.

## Before you start

1. The base configuration is built: `dunlin build`.
2. Evaluation data exists: `dunlin eval-data import cases.json` (`references/evaluation.md`).
3. You know the base's recall: `dunlin eval`. If it is already 1.000 on a handful of cases,
   the data cannot show an improvement. Add harder cases first.

Every trial uses the documents as they are in the store at that moment. The folder is not
scanned again, so that trials differ in their settings and in nothing else. If documents
changed, run `dunlin build` before optimizing.

## The instruction

```yaml
# tune.yaml
optimize:
  base: Dunlinfile.yaml
  vary:
    - name: chunking
      options:
        as-is: {}
        length:
          transform:
            - chunk.length: { unit: characters, size: [200, 400, 800], overlap: 40 }
        structure:
          transform:
            - chunk.structure: { max_chars: 400, context: { headings: true, prepend: true } }
    - name: search
      options:
        top10: {}
        top5:
          search:
            - retrieval.keyword: { top_k: 5 }
  eval:
    data: default
    split: dev
    metric: recall
  limit:
    candidates: 20
```

How it reads:

- **`base`** is the configuration every trial starts from, relative to the project folder.
- **`vary`** is a list of axes. Each axis has a `name` and `options`.
- **An option** is a name and the sections it replaces. A section named in an option
  replaces the base's section whole, so write every module the section needs. `{}` replaces
  nothing: it is "the base as it is" on that axis.
- **A list as an argument value** means "one trial for each value". `size: [200, 400, 800]`
  makes three options named `length(size=200)` and so on.
- **Trials are every combination** of one option per axis. The example has 5 chunking
  options times 2 search options: 10 trials, plus the base as trial 000.
- **`eval`** names the data and split for all trials. Left out, the base's `eval` section
  is used. `metric` is `recall`; nothing else is supported.
- **`limit.candidates`** runs only the first N trials in the order written (the base is not
  counted). The rest are listed as skipped and run the next time.

Rules that are checked before anything runs:

- An option can replace `transform`, `index` and `search`. Not `extract` (the documents are
  fixed) and not `eval` (every trial is measured the same way).
- Two axes cannot replace the same section. Things that change together go in one option.
- An option writes no `tag` and no `from`. The optimization names the trials itself.

A trial whose configuration cannot work (for example a module the staged style lacks) is
not an error of the instruction: it is listed as `invalid` with the reason and the others
run.

### Designing the axes

- Change few things. Two axes of three options are 9 trials you can read; four axes are a
  table nobody reads.
- Put the base on every axis (`as-is: {}`), so each change is seen alone as well as
  combined.
- Order options from most to least promising when a `limit` is set.
- `search`-only changes are almost free: they reuse the index and only evaluate. Chunking
  changes rebuild chunks and the index.

## Run

```sh
dunlin optimize tune.yaml
```

Progress goes to standard error, one block per trial. The name of the optimization is the
file's name without its extension (`tune`), or `--name <name>`.

```text
Optimization tune (recall on default, dev)
Trial  Recall  To base  Ran                   Time  What
000    0.720   -        eval                  0.1s  base
007    0.860   +0.140   transform+index+eval  0.4s  chunking=structure,search=top10
003    0.800   +0.080   transform+index+eval  0.3s  chunking=length(size=400),search=top10
002    0.640   -0.080   eval                  0.1s  chunking=as-is,search=top5
009    -       -        -                     -     chunking=..., (invalid: index.vector is not on the new store yet.)
The files of the trials are in .dunlin/trials/tune.
Every trial used the documents docs, version 6 (6 documents).
```

- Sorted best first. `To base` is the difference to trial 000.
- `Ran` says what the trial had to do. `nothing` means everything was there already.
- A trial that failed has no recall and says `failed:` with the reason. After three
  failures in a row the rest are not tried.

Running the same instruction again does no work and gives the same table. See it later with
`dunlin results tune`; from another terminal this also shows a run in progress.

## Look into trials

Each trial has one name, `<optimization>/<number>`, shared by its chunks, its index and its
result.

```sh
dunlin results tune/007                       # which cases trial 007 found and missed
dunlin search "<question>" --index tune/007   # what a user would get from it
dunlin eval .dunlin/trials/tune/007.yaml      # run one trial again by hand
```

A trial that only changed `search` has no index of its own: it used the base's.

## Choose

Do not pick on the first decimal. Before recommending a trial:

- **Is the difference more than a case or two?** With 25 cases, one case is 0.04.
- **Which cases changed?** Compare `dunlin results tune/000` with the candidate. A trial
  that fixes three cases and breaks two is a different thing from one that fixes one.
- **What does it cost?** Smaller chunks mean more chunks and a larger index; a larger
  `top_k` means more text for whatever reads the results.
- **Among equals, take the simplest**: the one closest to the base.

Then confirm on `test` once (`references/evaluation.md`), and report both numbers.

## Apply the chosen trial

A trial's settings live in its file, not in the project's configuration. Until they are
copied over, the next `dunlin build` uses the old settings.

1. Open `.dunlin/trials/<name>/<number>.yaml`.
2. Copy its `transform`, `index` and `search` sections into `Dunlinfile.yaml`, replacing
   the ones there.
3. In the copied sections, change each `- tag: <name>/<number>` back to the project's own
   tag, or remove the line.
4. Leave `extract`, `eval` and `serve` of `Dunlinfile.yaml` as they are. Do not copy the
   trial's `extract` (`- from: ...`): the project scans its folder.
5. Run `dunlin eval`. It should report the trial's recall, and reuse the trial's chunks and
   index if the documents have not changed.
6. Release when the user agrees: `dunlin release <the project's index tag, or default>`.

Editing `Dunlinfile.yaml` changes the project's settings for good. Show the user the diff
and the numbers before you do it, unless they already asked you to apply the winner.

## Not there yet

- Choosing values automatically (Bayesian optimization and the like). Every combination
  written in the instruction is tried.
- Metrics other than recall, and limits by time or cost.
- Options that change `extract`.
