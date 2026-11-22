# Comparing settings with an optimization (staged projects)

An optimization takes a base configuration and a list of things to vary, writes one
configuration file per combination, builds and evaluates each, and shows them side by side.
Use it instead of editing the configuration by hand and re-running: it keeps every trial,
reuses whatever two trials share, and measures all of them the same way.

## Before you start

1. The base configuration is built: `nest build`.
2. Evaluation data exists: `nest eval-data import cases.json` (`references/evaluation.md`).
3. You know the base's recall: `nest eval`. If it is already 1.000 on a handful of cases,
   the data cannot show an improvement. Add harder cases first.

Every trial uses the documents as they are in the store at that moment. The folder is not
scanned again, so that trials differ in their settings and in nothing else. If documents
changed, run `nest build` before optimizing.

## The instruction

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

### Letting the trials be chosen

Every combination is the default (`search: grid`). When there are too many to try, or a
number should be searched over a range, let Knowledge-Nest choose the trials:

```yaml
optimize:
  base: KnowledgeNestfile.yaml
  search: tpe          # or random
  seed: 1
  vary:
    - name: chunking
      options:
        length:
          transform:
            - chunk.length: { unit: characters, size: { int: [100, 1600], log: true }, overlap: 40 }
        structure:
          transform:
            - chunk.structure: { max_chars: { int: [200, 1600] } }
    - name: search
      options:
        keyword:
          search:
            - retrieval.keyword: { top_k: [3, 5, 10] }
  limit:
    candidates: 30
```

- **`{ int: [low, high] }`** is any whole number in the range. `log: true` spreads the tries
  evenly on a logarithmic scale, which suits sizes: 100 to 200 counts as much as 800 to 1600.
  A range is only for `random` and `tpe`.
- **A list** is a choice among its values, and the options of an axis are a choice among
  them. A range inside an option is only tried when that option is.
- **`random`** draws every trial at random. **`tpe`** draws the first few at random, then
  chooses each next trial from the results so far: values that did well are tried more.
- **`limit.candidates` is required** and is the number of trials, besides the base.
- **`seed`** fixes the draws. The same instruction and seed give the same trials, so running
  it again does no work. Change the seed to explore other trials.
- A trial that was tried already is drawn again, so the budget is not spent twice.

Use `grid` while there are a dozen combinations or fewer: it is complete and easy to read.
Use `tpe` with ranges and 20 or more trials. With fewer, `tpe` is no better than `random`,
because it needs results to learn from.

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
nest optimize tune.yaml
```

Progress goes to standard error, one block per trial. The name of the optimization is the
file's name without its extension (`tune`), or `--name <name>`.

```text
Optimization tune (recall, then MRR, on default, dev)
Trial  Recall  MRR    To base  Ran                   Time  What
000    0.720   0.610  -        eval                  0.1s  base
007    0.860   0.700  +0.140   transform+index+eval  0.4s  chunking=structure,search=top10
003    0.800   0.640  +0.080   transform+index+eval  0.3s  chunking=length(size=400),search=top10
002    0.640   0.640  -0.080   eval                  0.1s  chunking=as-is,search=top5
009    -       -      -        -                     -     chunking=..., (invalid: index.vector is not on the new store yet.)
The files of the trials are in .knowledge-nest/trials/tune.
Every trial used the documents docs, version 6 (6 documents).
```

- Sorted best first: by recall, and by MRR when the recall is the same. `To base` is the
  recall's difference to trial 000.
- **MRR** (mean reciprocal rank) counts 1 for evidence returned first, 0.5 for second, 0.33
  for third, and nothing for a miss. Two settings with the same recall differ in MRR when one
  puts the evidence higher. When every trial has the same recall, MRR is what separates them.
- `Ran` says what the trial had to do. `nothing` means everything was there already.
- A trial that failed has no recall and says `failed:` with the reason. After three
  failures in a row the rest are not tried.

Running the same instruction again does no work and gives the same table. See it later with
`nest results tune`; from another terminal this also shows a run in progress.

## Look into trials

Each trial has one name, `<optimization>/<number>`, shared by its chunks, its index and its
result.

```sh
nest results tune/007                       # which cases trial 007 found and missed
nest search "<question>" --index tune/007   # what a user would get from it
nest eval .knowledge-nest/trials/tune/007.yaml      # run one trial again by hand
```

A trial that only changed `search` has no index of its own: it used the base's.

## Choose

Do not pick on the first decimal. Before recommending a trial:

- **Is the difference more than a case or two?** With 25 cases, one case is 0.04 of recall.
  A difference only in MRR is weaker evidence than one in recall: say which it is.
- **Which cases changed?** Compare `nest results tune/000` with the candidate. A trial
  that fixes three cases and breaks two is a different thing from one that fixes one.
- **What does it cost?** Smaller chunks mean more chunks and a larger index; a larger
  `top_k` means more text for whatever reads the results.
- **Among equals, take the simplest**: the one closest to the base.

Then confirm on `test` once (`references/evaluation.md`), and report both numbers.

## Apply the chosen trial

A trial's settings live in its file, not in the project's configuration. Until they are
copied over, the next `nest build` uses the old settings.

1. Open `.knowledge-nest/trials/<name>/<number>.yaml`.
2. Copy its `transform`, `index` and `search` sections into `KnowledgeNestfile.yaml`, replacing
   the ones there.
3. In the copied sections, change each `- tag: <name>/<number>` back to the project's own
   tag, or remove the line.
4. Leave `extract`, `eval` and `serve` of `KnowledgeNestfile.yaml` as they are. Do not copy the
   trial's `extract` (`- from: ...`): the project scans its folder.
5. Run `nest eval`. It should report the trial's recall, and reuse the trial's chunks and
   index if the documents have not changed.
6. Release when the user agrees: `nest release <the project's index tag, or default>`.

Editing `KnowledgeNestfile.yaml` changes the project's settings for good. Show the user the diff
and the numbers before you do it, unless they already asked you to apply the winner.

## Not there yet

- Optimizing several measures at once (recall against index size or time), and stopping a
  trial early when it is going badly.
- Measures other than recall and MRR, and limits by time or cost.
- Options that change `extract`.
