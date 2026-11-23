// Measures search quality on a fixed set of documents and evaluation cases (docs/benchmarks.md).
//
//   node benches/retrieval.mjs [config.yaml ...]
//
// Each configuration is built in a new project in a temporary folder from benches/retrieval/corpus,
// the cases of benches/retrieval/cases.jsonl are imported, and the dev split is evaluated. The
// first configuration is the base the others are compared with. Without arguments, keyword.yaml
// and hybrid.yaml are run. hybrid.yaml needs ruri-v3 in Ollama.
import { execFile } from 'node:child_process';
import { cp, mkdtemp, readFile, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const here = fileURLToPath(new URL('retrieval/', import.meta.url));
const cli = fileURLToPath(new URL('../tools/rag/cli.mjs', import.meta.url));
const configs = process.argv.slice(2).length
  ? process.argv.slice(2).map((file) => resolve(file))
  : [join(here, 'keyword.yaml'), join(here, 'hybrid.yaml')];
const temp = await realpath(tmpdir());
const root = await realpath(await mkdtemp(join(temp, 'knowledge-nest-retrieval-bench-')));
if (dirname(root) !== temp || !basename(root).startsWith('knowledge-nest-retrieval-bench-')) throw new Error('Unexpected temporary directory.');
const exec = promisify(execFile);
const env = { ...process.env, KNOWLEDGE_NEST_GLOBAL_CONFIG: join(root, 'empty-global.yaml') };
async function run(cwd, ...args) {
  let stdout;
  try {
    ({ stdout } = await exec(process.execPath, [cli, ...args, '--json'], { cwd, env, maxBuffer: 256 * 1024 * 1024 }));
  } catch (error) {
    stdout = error.stdout;
  }
  const result = JSON.parse(stdout);
  if (result.status !== 'succeeded') throw new Error(`nest ${args[0]}: ${JSON.stringify(result.errors)}`);
  return result.data;
}

const fixed = (value) => (typeof value === 'number' ? value.toFixed(3) : String(value));
const shown = ['evidence_recall@context', 'evidence_recall@1', 'evidence_recall@3', 'evidence_recall@10', 'mrr@10', 'ndcg@10', 'evidence_share@context'];
try {
  const reports = [];
  for (const [i, config] of configs.entries()) {
    const name = basename(config).replace(/\.ya?ml$/u, '');
    const project = join(root, `${i}-${name}`);
    await run(root, 'init', 'rag', project);
    await cp(join(here, 'corpus'), join(project, 'docs'), { recursive: true });
    await writeFile(join(project, 'KnowledgeNestfile.yaml'), await readFile(config));
    const started = Date.now();
    const build = await run(project, 'build');
    const seconds = (Date.now() - started) / 1000;
    const dataset = await run(project, 'eval-data', 'import', join(here, 'cases.jsonl'));
    await run(project, 'eval-data', 'use', dataset.id);
    const report = await run(project, 'eval', '--tag', name);
    reports.push({ name, project, report });
    console.log(`\n## ${name}  (${report.rows.length} cases, build ${seconds.toFixed(1)} s, ${build.documents} documents)`);
    console.log(shown.filter((key) => key in report.scores).map((key) => `${key} ${fixed(report.scores[key])}`).join('  '));
    console.log('latency_ms', JSON.stringify(report.latency_ms), ' context chars', fixed(report.context.average_chars));
    console.log('stages.recall', JSON.stringify(Object.fromEntries(Object.entries(report.stages.recall).map(([key, value]) => [key, fixed(value)]))));
    console.log('stages.lost', JSON.stringify(report.stages.lost));
    for (const [group, entries] of Object.entries(report.breakdown)) {
      for (const [key, value] of Object.entries(entries)) {
        console.log(`${group}.${key}  n=${value.cases}  overlap ${fixed(value.overlap)}  `
          + ['evidence_recall@context', 'evidence_recall@1', 'mrr@10'].map((score) => `${score} ${fixed(value.scores[score])}`).join('  '));
      }
    }
  }
  const [base, ...others] = reports;
  // The two-sided exact sign test of `report compare`, for reports of different projects.
  const signTest = (wins, losses) => {
    const n = wins + losses;
    if (!n) return null;
    let tail = 0;
    let term = 0.5 ** n;
    for (let k = 0; k <= Math.min(wins, losses); k++) {
      tail += term;
      term *= (n - k) / (k + 1);
    }
    return Math.min(1, 2 * tail);
  };
  for (const other of others) {
    const rows = new Map(base.report.rows.map((row) => [row.id, row]));
    console.log(`\n## ${other.name} - ${base.name}`);
    for (const key of shown.filter((score) => score in other.report.scores && score in base.report.scores)) {
      let wins = 0;
      let losses = 0;
      for (const row of other.report.rows) {
        const before = rows.get(row.id)?.scores[key];
        if (before === undefined) continue;
        if (row.scores[key] > before) wins++;
        else if (row.scores[key] < before) losses++;
      }
      console.log(`${key}  ${fixed(other.report.scores[key] - base.report.scores[key])}  better ${wins}  worse ${losses}  p ${fixed(signTest(wins, losses))}`);
    }
  }
} finally {
  if (await realpath(root) === root) await rm(root, { recursive: true, force: true, maxRetries: 5 });
}
