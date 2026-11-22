#!/usr/bin/env node
// Writes an HTML report of Knowledge-Nest evaluation results (staged projects).
//
//   node report.mjs [<name>] [--out <file>] [--project <folder>] [--title <text>]
//
// Without a name: every named result. With the name of an optimization: its trials and a
// table of cases by trial. With the name of a result: its cases with what was returned.
//
// It reads everything from `nest results ... --json`, so it shows what the store holds
// and nothing else. Set KNOWLEDGE_NEST to the command that runs Knowledge-Nest when it is not `nest`.

import { spawnSync } from 'node:child_process';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';

const args = process.argv.slice(2);
const options = {};
const positionals = [];
for (let index = 0; index < args.length; index += 1) {
  const arg = args[index];
  if (arg.startsWith('--')) {
    options[arg.slice(2)] = args[index + 1];
    index += 1;
  } else {
    positionals.push(arg);
  }
}
const name = positionals[0];
const project = resolve(options.project ?? '.');
const out = resolve(options.out ?? 'knowledge-nest-report.html');

function fail(message) {
  process.stderr.write(`${message}\n`);
  process.exit(1);
}

const quote = (value) => `"${String(value).replaceAll('"', '\\"')}"`;

/** Runs `nest results` and gives its data, or stops with what Knowledge-Nest said. */
function results(of) {
  const command = [
    process.env.KNOWLEDGE_NEST ?? 'nest',
    'results',
    ...(of === undefined ? [] : [quote(of)]),
    '--project',
    quote(project),
    '--json',
  ].join(' ');
  const ran = spawnSync(command, { shell: true, encoding: 'utf8', maxBuffer: 256 * 1024 * 1024 });
  if (ran.error) fail(`Knowledge-Nest could not be run: ${ran.error.message}`);
  let answer;
  try {
    answer = JSON.parse(ran.stdout);
  } catch {
    fail(`Knowledge-Nest did not answer with JSON.\n${ran.stderr || ran.stdout}`);
  }
  if (answer.status !== 'succeeded') {
    fail((answer.errors ?? []).map((error) => error.message).join('\n') || 'Knowledge-Nest failed.');
  }
  return answer.data;
}

const escape = (value) =>
  String(value ?? '')
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;');
const percent = (value) => (typeof value === 'number' ? `${(value * 100).toFixed(1)}%` : '–');
const mrr = (value) => (typeof value === 'number' ? value.toFixed(3) : '–');
const signed = (value) => `${value >= 0 ? '+' : '−'}${(Math.abs(value) * 100).toFixed(1)}`;
const bar = (value) =>
  typeof value === 'number'
    ? `<span class="bar"><span style="width:${(value * 100).toFixed(1)}%"></span></span>`
    : '';
const place = (hit) => `${escape(hit.source)}/${escape(hit.document)}`;
const lines = (hit) => (hit.lines ? ` <span class="dim">lines ${hit.lines[0]}–${hit.lines[1]}</span>` : '');

function stat(label, value) {
  return `<div class="stat"><div class="value">${value}</div><div class="label">${escape(label)}</div></div>`;
}

/** The cases of one result: the missed ones first, each with what was expected and returned. */
function casesSection(result) {
  const rows = result.cases
    .map((item) => {
      const missed = item.rank === null || item.rank === undefined;
      const expected = item.expected
        ? `<div>${place(item.expected)}</div><blockquote>${escape(item.expected.quote)}</blockquote>`
        : '<span class="dim">not in the evaluation data any more</span>';
      const returned = (item.returned ?? [])
        .map((hit, position) => {
          const right =
            item.expected &&
            hit.source === item.expected.source &&
            hit.document === item.expected.document;
          const mark = position + 1 === item.rank ? ' class="hit"' : right ? ' class="near"' : '';
          return `<li${mark}>${place(hit)}${lines(hit)}</li>`;
        })
        .join('');
      return `<tr class="${missed ? 'missed' : 'found'}">
<td class="rank">${missed ? 'missed' : item.rank}</td>
<td><div class="question">${escape(item.question)}</div><div class="dim">${escape(item.id)}</div></td>
<td>${expected}</td>
<td>${returned ? `<ol>${returned}</ol>` : '<span class="dim">nothing</span>'}</td>
</tr>`;
    })
    .join('\n');
  return `<table class="cases">
<thead><tr><th>Found at</th><th>Question</th><th>Expected evidence</th><th>Returned, in order</th></tr></thead>
<tbody>
${rows}
</tbody></table>
<p class="dim">In "Returned", the chunk that holds the evidence is marked. A chunk of the right document
that does not hold the quote is marked lightly: the document was found and the passage was not.</p>`;
}

function resultPage(result) {
  const names = result.names?.length ? result.names.join(', ') : `version ${result.version}`;
  const missed = result.cases.filter((item) => item.rank === null || item.rank === undefined).length;
  return {
    title: `Evaluation result ${names}`,
    body: `<div class="stats">
${stat('recall', percent(result.recall))}
${stat('MRR', mrr(result.mrr))}
${stat('cases found', `${result.found} of ${result.cases.length}`)}
${stat('cases missed', missed)}
</div>
<p>Index <b>${escape(result.from?.[0] ?? '–')}</b>, evaluation data <b>${escape(result.from?.[1] ?? '–')}</b>.
Result version ${result.version}, made ${escape(result.made ?? '')}.</p>
<h2>Cases</h2>
${casesSection(result)}`,
  };
}

function overviewPage(data) {
  const rows = data.entries
    .map(
      (entry) => `<tr>
<td>${escape(entry.names.join(', ') || '–')}</td>
<td class="num">${percent(entry.recall)}</td>
<td>${bar(entry.recall)}</td>
<td class="num">${mrr(entry.mrr)}</td>
<td class="num">${entry.found} of ${entry.cases}</td>
<td>${escape(entry.from?.[0] ?? '–')}</td>
<td>${escape(entry.from?.[1] ?? '–')}</td>
<td class="dim">v${entry.version}, ${escape(entry.made)}</td>
</tr>`,
    )
    .join('\n');
  const optimizations = (data.optimizations ?? []).length
    ? `<p>Optimizations: ${data.optimizations.map((item) => `<b>${escape(item)}</b>`).join(', ')}.</p>`
    : '';
  return {
    title: 'Evaluation results',
    body: `${optimizations}
<table>
<thead><tr><th>Result</th><th>Recall</th><th></th><th>MRR</th><th>Found</th><th>Index</th><th>Evaluation data</th><th>Version</th></tr></thead>
<tbody>
${rows || '<tr><td colspan="8" class="dim">No result has a name yet. Run nest eval.</td></tr>'}
</tbody></table>`,
  };
}

function optimizationPage(record) {
  const trials = record.candidates;
  const base = trials.find((trial) => trial.number === 0);
  const number = (trial) => String(trial.number).padStart(3, '0');
  // By recall, then by MRR, as the command line orders them.
  const ranked = [...trials].sort(
    (left, right) =>
      (right.recall ?? -1) - (left.recall ?? -1) ||
      (right.mrr ?? -1) - (left.mrr ?? -1) ||
      left.number - right.number,
  );
  const trialRows = ranked
    .map((trial) => {
      const evaluated = typeof trial.recall === 'number';
      const toBase =
        evaluated && base && typeof base.recall === 'number' && trial.number !== 0
          ? signed(trial.recall - base.recall)
          : '–';
      const what = evaluated
        ? escape(trial.name)
        : `${escape(trial.name)} <span class="reason">${escape(trial.state)}${trial.reason ? `: ${escape(trial.reason)}` : ''}</span>`;
      return `<tr class="${evaluated ? '' : 'unrun'}">
<td>${number(trial)}</td>
<td class="num">${percent(trial.recall)}</td>
<td>${bar(trial.recall)}</td>
<td class="num">${evaluated ? mrr(trial.mrr) : '–'}</td>
<td class="num">${toBase}</td>
<td>${escape((trial.ran ?? []).join(' + ') || (evaluated ? 'nothing' : '–'))}</td>
<td class="num">${typeof trial.elapsed_ms === 'number' && evaluated ? `${(trial.elapsed_ms / 1000).toFixed(1)}s` : '–'}</td>
<td>${what}</td>
</tr>`;
    })
    .join('\n');

  // The cases of each evaluated trial. Trials that share a result are asked once.
  const reports = new Map();
  for (const trial of trials) {
    if (typeof trial.report === 'number' && !reports.has(trial.report)) {
      reports.set(trial.report, results(trial.report));
    }
  }
  const evaluated = ranked.filter((trial) => reports.has(trial.report));
  const cases = new Map();
  for (const report of reports.values()) {
    for (const item of report.cases) {
      if (!cases.has(item.id)) cases.set(item.id, item.question);
    }
  }
  const rankOf = (trial, id) => reports.get(trial.report).cases.find((item) => item.id === id)?.rank;
  const missedBy = (id) =>
    evaluated.filter((trial) => {
      const rank = rankOf(trial, id);
      return rank === null || rank === undefined;
    }).length;
  const caseIds = [...cases.keys()].sort((left, right) => missedBy(right) - missedBy(left));
  const matrix = caseIds
    .map((id) => {
      const cells = evaluated
        .map((trial) => {
          const rank = rankOf(trial, id);
          return rank === null || rank === undefined
            ? '<td class="cell miss" title="missed">×</td>'
            : `<td class="cell ok" title="found at ${rank}">${rank}</td>`;
        })
        .join('');
      return `<tr><td><div class="question">${escape(cases.get(id))}</div><div class="dim">${escape(id)}</div></td><td class="num">${missedBy(id)}</td>${cells}</tr>`;
    })
    .join('\n');

  const settings = [...trials]
    .sort((left, right) => left.number - right.number)
    .map((trial) => {
      const file = resolve(project, trial.file ?? '');
      const text = trial.file && existsSync(file) ? readFileSync(file, 'utf8') : null;
      return `<details><summary>${number(trial)} ${escape(trial.name)}</summary>
${text === null ? `<p class="dim">${escape(trial.file)} is not there any more.</p>` : `<pre>${escape(text)}</pre>`}
</details>`;
    })
    .join('\n');

  const best = ranked.find((trial) => typeof trial.recall === 'number');
  const documents = record.documents;
  return {
    title: `Optimization ${record.name}`,
    body: `<div class="stats">
${stat('best recall', best ? percent(best.recall) : '–')}
${stat('base recall', base ? percent(base.recall) : '–')}
${stat('trials evaluated', `${trials.filter((trial) => typeof trial.recall === 'number').length} of ${trials.length}`)}
</div>
<p>Ordered by recall, then MRR, on evaluation data <b>${escape(record.data)}</b>, split <b>${escape(record.split)}</b>.
${record.search && record.search !== 'grid' ? `Trials chosen by <b>${escape(record.search)}</b>, seed ${record.seed}.` : ''}
${record.stopped ? `<span class="reason">${escape(record.stopped)}</span>` : ''}
${documents ? `Every trial used the documents <b>${escape(documents.name)}</b>, version ${documents.version} (${documents.count} documents).` : ''}</p>
${record.running ? '<p class="reason">This optimization is still running: the report shows what is done so far.</p>' : ''}
<h2>Trials</h2>
<table>
<thead><tr><th>Trial</th><th>Recall</th><th></th><th>MRR</th><th>To base</th><th>Ran</th><th>Time</th><th>What changed</th></tr></thead>
<tbody>
${trialRows}
</tbody></table>
<h2>Cases by trial</h2>
<p class="dim">Each cell is the place the evidence was returned at; × is a miss. Cases missed by the most trials come first. Columns are in the order of the table above.</p>
<div class="scroll"><table class="matrix">
<thead><tr><th>Question</th><th>Missed by</th>${evaluated.map((trial) => `<th title="${escape(trial.name)}">${number(trial)}</th>`).join('')}</tr></thead>
<tbody>
${matrix}
</tbody></table></div>
<h2>Settings of each trial</h2>
${settings}`,
  };
}

function page({ title, body }) {
  const heading = options.title ?? title;
  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${escape(heading)}</title>
<style>
:root { --bg:#fff; --fg:#1b1f24; --dim:#667085; --line:#e4e7ec; --soft:#f6f7f9; --good:#12805c; --good-bg:#e6f4ee; --bad:#b42318; --bad-bg:#fdecea; --near-bg:#fff4d6; --accent:#2f6fde; }
@media (prefers-color-scheme: dark) {
  :root { --bg:#14171c; --fg:#e6e8eb; --dim:#98a2b3; --line:#2a2f37; --soft:#1c2027; --good:#4fd1a1; --good-bg:#12332a; --bad:#ff8a80; --bad-bg:#3a1c1a; --near-bg:#3a3214; --accent:#7aa7ff; }
}
* { box-sizing: border-box; }
body { margin:0; background:var(--bg); color:var(--fg); font:15px/1.55 system-ui, -apple-system, "Segoe UI", "Hiragino Sans", "Yu Gothic UI", Meiryo, sans-serif; }
main { max-width:1100px; margin:0 auto; padding:32px 16px 64px; }
h1 { font-size:26px; margin:0 0 4px; }
h2 { font-size:18px; margin:36px 0 10px; }
.dim { color:var(--dim); font-size:13px; }
.reason { color:var(--bad); font-size:13px; }
.stats { display:flex; flex-wrap:wrap; gap:12px; margin:20px 0; }
.stat { background:var(--soft); border:1px solid var(--line); border-radius:8px; padding:12px 18px; min-width:150px; }
.stat .value { font-size:24px; font-weight:600; font-variant-numeric:tabular-nums; }
.stat .label { color:var(--dim); font-size:13px; }
.scroll { overflow-x:auto; }
table { border-collapse:collapse; width:100%; }
th, td { text-align:left; vertical-align:top; padding:8px 10px; border-bottom:1px solid var(--line); }
th { font-size:12px; text-transform:uppercase; letter-spacing:.04em; color:var(--dim); font-weight:600; white-space:nowrap; }
.num { text-align:right; font-variant-numeric:tabular-nums; white-space:nowrap; }
.bar { display:inline-block; width:120px; height:8px; background:var(--line); border-radius:4px; overflow:hidden; vertical-align:middle; }
.bar span { display:block; height:100%; background:var(--accent); }
tr.unrun td { color:var(--dim); }
.question { font-weight:600; }
.rank { font-variant-numeric:tabular-nums; white-space:nowrap; font-weight:600; }
tr.missed .rank { color:var(--bad); }
tr.found .rank { color:var(--good); }
blockquote { margin:4px 0 0; padding:4px 10px; border-left:3px solid var(--line); color:var(--dim); font-size:13px; }
ol { margin:0; padding-left:20px; font-size:13px; }
li.hit { background:var(--good-bg); border-radius:4px; padding:0 4px; }
li.near { background:var(--near-bg); border-radius:4px; padding:0 4px; }
.matrix .cell { text-align:center; font-variant-numeric:tabular-nums; min-width:44px; }
.cell.ok { background:var(--good-bg); color:var(--good); }
.cell.miss { background:var(--bad-bg); color:var(--bad); font-weight:700; }
details { border:1px solid var(--line); border-radius:8px; margin:8px 0; padding:8px 12px; }
summary { cursor:pointer; font-weight:600; }
pre { background:var(--soft); padding:12px; border-radius:6px; overflow-x:auto; font-size:13px; }
footer { margin-top:48px; color:var(--dim); font-size:12px; }
</style>
</head>
<body>
<main>
<h1>${escape(heading)}</h1>
<div class="dim">${escape(project)}</div>
${body}
<footer>Made from <code>nest results${name === undefined ? '' : ` ${escape(name)}`} --json</code> on ${new Date().toISOString().slice(0, 16).replace('T', ' ')}Z.</footer>
</main>
</body>
</html>
`;
}

const data = results(name);
const made = Array.isArray(data.candidates)
  ? optimizationPage(data)
  : Array.isArray(data.cases)
    ? resultPage(data)
    : overviewPage(data);
writeFileSync(out, page(made));
process.stdout.write(`${out}\n`);
