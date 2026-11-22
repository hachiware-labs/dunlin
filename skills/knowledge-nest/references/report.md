# HTML reports (staged projects)

`scripts/report.mjs` writes one self-contained HTML file from what `nest results --json`
gives. Use it instead of writing HTML by hand: it shows exactly what the store holds, and
the same question gets the same report every time.

```sh
node <skill folder>/scripts/report.mjs [<name>] --out <file> [--project <folder>] [--title <text>]
```

| `<name>` | The report |
| --- | --- |
| left out | Every named result, best first |
| an optimization (`tune`) | The trials, a table of cases by trial, and the settings of each trial |
| a result (`default`, `tune/007`) | Its cases: question, expected evidence, and what was returned in order |

- `--out` defaults to `knowledge-nest-report.html` in the current folder. Put reports where the
  user can open them, such as the project folder or a `reports/` folder in it, and tell
  them the path.
- `--project` defaults to the current folder.
- The script runs `nest`. If Knowledge-Nest is started another way here, set the environment
  variable `KNOWLEDGE_NEST` to that command.
- It needs Node, which Knowledge-Nest itself needs.
- It prints the path of the file it wrote. An error from Knowledge-Nest is printed as it is.

## Which report to make

- After `nest optimize`: the optimization report. Its "Cases by trial" table is what the
  terminal cannot show: which questions every trial misses (a problem of the documents or
  of the evaluation data, not of the settings) and which only some miss (what the settings
  decide).
- After `nest eval`, or to explain missed questions: the result report for `default` or
  for a trial. Look at "Returned": a marked chunk of the right document without the quote
  means the document was found and the passage was not, which points at chunking.
- To show where a project stands: the overview.

## Going with the report

The report shows data, not conclusions. In your message, give the two or three things that
matter: the best trial and how much better it is than the base, the questions no trial
finds, and what you recommend. Point to the file for the rest.

## When the user wants something else in it

The script covers the common reports. For a one-off need, such as two results side by side
or a chart, get the data yourself and write the page:

```sh
nest results <name> --json
```

- For a result, `data.cases[]` has `id`, `question`, `rank` (null when missed), `expected`
  (`source`, `document`, `quote`) and `returned[]` (`source`, `document`, `lines`).
- For an optimization, `data.candidates[]` has `number`, `name`, `choices`, `state`,
  `reason`, `recall`, `ran`, `elapsed_ms`, `tag`, `file` and `report`. `report` is the
  version of the trial's result: `nest results <report> --json` gives its cases.
- Without a name, `data.entries[]` has `names`, `version`, `recall`, `found`, `cases`,
  `from` and `made`.

Keep such a page self-contained (no files loaded from the network), so that it opens
anywhere and can be passed on.
