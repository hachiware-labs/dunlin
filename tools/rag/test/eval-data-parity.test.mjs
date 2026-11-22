// Spec 28.11 step 4e: eval-data commands in Rust give the outputs, saved datasets, tags and
// selections of the JavaScript code they replaced (test/fixtures/eval-data-reference.mjs). Two
// copies of one built project run the same commands, one through the CLI and one through the
// reference. Times differ between the runs, so dataset IDs are compared by order of appearance.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFile } from 'node:child_process';
import { cp, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { DatabaseSync } from 'node:sqlite';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { parse } from 'yaml';

import { referenceEnsureEvaluation, referenceManageEvaluationData } from './fixtures/eval-data-reference.mjs';
import { referenceOpen } from './fixtures/search-reference.mjs';

const cli = fileURLToPath(new URL('../cli.mjs', import.meta.url));
const runNode = promisify(execFile);
const sha256 = (text) => createHash('sha256').update(text).digest('hex');

const documents = {
  'guide.md': '# 手順\n\nポンプの交換手順です。\n\n## 点検\n\n月に一度ポンプを点検します。\nフィルターは６か月ごとに交換する。\n\n'
    + '## 清掃\n\n月に一度ポンプを点検します。\n',
  'notes.txt': '保守の記録\n2024年4月にポンプの点検を行った。\n異音があったため、ファンを交換した。\n',
  'other.md': '# 照明\n\n照明の交換は年に一度です。\n',
};

/**
 * Replaces what differs between the two runs in JSON text: times, dataset IDs, and document
 * versions, which include the location of each copy.
 */
function normalizer() {
  const ids = new Map();
  const versions = new Map();
  const placeholder = (map, prefix) => (value) => {
    if (!map.has(value)) map.set(value, prefix + map.size);
    return map.get(value);
  };
  return (text) => text.replace(/"(created_at|at)":"[^"]*"/gu, '"$1":"time"')
    .replace(/ed-[0-9a-f]{64}/gu, placeholder(ids, 'dataset-'))
    .replace(/(?<="version":")[0-9a-f]{64}/gu, placeholder(versions, 'version-'));
}

test('manages evaluation data like the former JavaScript code', { timeout: 300_000 }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'knowledge-nest-eval-data-parity-'));
  const env = { ...process.env, KNOWLEDGE_NEST_GLOBAL_CONFIG: join(root, 'global.yaml') };
  await runNode(process.execPath, [cli, 'init', 'rag', 'kb', '--no-embedding'], { cwd: root, env });
  const base = join(root, 'kb');
  for (const [name, content] of Object.entries(documents)) await writeFile(join(base, 'docs', name), content);
  await runNode(process.execPath, [cli, '--project', 'kb', 'build'], { cwd: root, env });
  await cp(base, join(root, 'ref'), { recursive: true, preserveTimestamps: true });
  // Document versions include the file location, so the copy gets its own versions once.
  await runNode(process.execPath, [cli, '--project', 'ref', 'build'], { cwd: root, env });
  const run = (project, ...args) => runNode(process.execPath, [cli, '--project', project, ...args, '--json'], { cwd: root, env, encoding: 'utf8', timeout: 60_000 })
    .then(({ stdout }) => JSON.parse(stdout), (error) => ({ exitCode: error.code, ...JSON.parse(error.stdout) }));

  const refBase = join(root, 'ref');
  const paths = { data: join(refBase, '.knowledge-nest'), state: join(refBase, '.knowledge-nest', 'state.sqlite'),
    artifacts: join(refBase, '.knowledge-nest', 'artifacts'), corpus: join(refBase, '.knowledge-nest', 'corpus') };
  const db = new DatabaseSync(paths.state);
  db.exec('PRAGMA busy_timeout = 500');
  const actualDb = new DatabaseSync(join(base, '.knowledge-nest', 'state.sqlite'), { readOnly: true });
  // The databases close first, which Windows needs before removing their files.
  t.after(async () => {
    db.close();
    actualDb.close();
    await rm(root, { recursive: true, force: true, maxRetries: 5 });
  });
  const project = async () => {
    const configPath = join(refBase, 'KnowledgeNestfile.yaml');
    const text = await readFile(configPath, 'utf8');
    return { root: refBase, config: parse(text), configPath, configDigest: sha256(text), paths };
  };
  const open = () => referenceOpen(paths, JSON.parse(db.prepare('SELECT manifest FROM project JOIN builds ON builds.id = project.build_id').get().manifest));
  const readOnly = (action, values) => ['show', 'list', 'export', 'diff'].includes(action) || (action === 'review' && !values.case && !values.state);
  const reference = async (action, positionals, values = {}) => {
    if (!readOnly(action, values)) referenceEnsureEvaluation(db);
    return referenceManageEvaluationData(db, await project(), action, positionals, values, open);
  };
  const [actualIds, expectedIds] = [normalizer(), normalizer()];
  const selected = async (directory) => parse(await readFile(join(root, directory, 'KnowledgeNestfile.yaml'), 'utf8'))['eval-data']?.use ?? null;

  /** Dataset IDs by name on each side, so that commands can refer to them as `@name`. */
  const named = { kb: {}, ref: {} };
  const resolve = (side, value) => typeof value === 'string' && value.startsWith('@') ? named[side][value.slice(1)] : value;

  /**
   * Runs one command on both projects and compares the outputs, or the errors. `positionals` and
   * the CLI arguments may name datasets as `@name`. Returns both outputs.
   */
  const same = async (args, positionals, values = {}) => {
    const actual = await run('kb', 'eval-data', ...args.map((arg) => resolve('kb', arg)));
    let expected;
    let failure;
    try {
      expected = await reference(args[0], positionals.map((arg) => resolve('ref', arg)),
        Object.fromEntries(Object.entries(values).map(([key, value]) => [key, resolve('ref', value)])));
    } catch (error) {
      failure = error;
    }
    const label = args.join(' ');
    if (failure) {
      assert.equal(actual.status, 'failed', label + ': ' + failure.message);
      assert.equal(actual.errors[0].code, failure.code, label);
      assert.equal(actualIds(actual.errors[0].message), expectedIds(failure.message), label);
      assert.equal(actual.exitCode, failure.exitCode, label);
      return null;
    }
    assert.equal(actual.status, 'succeeded', label + ' ' + JSON.stringify(actual.errors));
    assert.equal(actualIds(JSON.stringify(actual.data)), expectedIds(JSON.stringify(expected)), label);
    assert.equal(actualIds(String(await selected('kb'))), expectedIds(String(await selected('ref'))), label + ' selection');
    return { actual: actual.data, expected };
  };
  const name = (key, result) => {
    named.kb[key] = result.actual.id;
    named.ref[key] = result.expected.id;
  };
  const rows = (database, table) => database.prepare(`SELECT * FROM ${table} ORDER BY rowid`).all();

  const imported = join(root, 'cases.jsonl');
  await writeFile(imported, [
    { question: 'ポンプの交換', doc: 'guide.md', quote: 'ポンプの交換手順です。' },
    { question: 'フィルター', doc: 'docs:guide.md', quote: 'フィルターは6か月ごとに交換する。', group: 'filters', split: 'test',
      created_at: '2026-01-01T00:00:00.000Z' },
    { question: '清掃', evidence: [{ doc: 'docs:guide.md', quote: '月に一度ポンプを点検します。', heading: '清掃' },
      { doc: 'notes.txt', quote: '異音があったため、ファンを交換した。' }], input_by: 'model', state: 'generated',
    generation: { model: 'local' }, created_at: '2026-01-02T00:00:00.000Z' },
    { question: '点検の前', doc: 'docs:guide.md', quote: '月に一度ポンプを点検します。', prefix: '## 点検', reason: 'kept' },
    { question: '古い', evidence: [{ doc: 'docs:gone.md', quote: '消えた', version: 'a'.repeat(64), original_hash: 'b'.repeat(64) }],
      state: 'needs_review', created_at: '2026-01-03T00:00:00.000Z' },
    { question: 'ID', doc: 'docs:other.md', quote: '照明の交換は年に一度です。', id: 'case-' + '1'.repeat(64), split: 'dev' },
  ].map((item) => JSON.stringify(item)).join('\r\n') + '\n\n');
  const first = await same(['import', imported], [imported]);
  name('first', first);
  assert.equal(first.actual.cases, 6);
  const add = (values) => same(['add', ...Object.entries(values).flatMap(([key, value]) => ['--' + key, value])], [], values);
  await add({ question: '照明', doc: 'other.md', quote: '照明の交換は年に一度です。', page: '1' });
  await add({ question: '点検の記録', doc: 'notes.txt', quote: '2024年4月にポンプの点検を行った。' });
  await add({ question: '点検', doc: 'docs:guide.md', quote: '月に一度ポンプを点検します。' });
  await add({ question: '架空', doc: 'docs:guide.md', quote: '月に二度ポンプを点検します。' });
  await add({ question: 'ポンプの交換', doc: 'guide.md', quote: 'ポンプの交換手順です。' });
  await add({ question: '頁', doc: 'guide.md', quote: 'ポンプの交換手順です。', page: 'x' });
  // Each copy saves its own search, whose evidence names that copy's document versions.
  for (const side of ['kb', 'ref']) {
    named[side].search = join(root, 'search-' + side + '.json');
    await writeFile(named[side].search, (await runNode(process.execPath, [cli, '--project', side, 'search', '異音', '--json'],
      { cwd: root, env, encoding: 'utf8' })).stdout);
  }
  name('latest', await add({ question: '異音', 'from-search': '@search', hit: '1' }));
  await add({ question: '異音', 'from-search': '@search', hit: '9' });
  const broken = join(root, 'broken.jsonl');
  await writeFile(broken, '{"question":"a","doc":"guide.md","quote":"ポンプの交換手順です。"}\n{oops\n');
  await same(['import', broken], [broken]);

  assert.equal((await same(['list'], [])).actual.datasets[0].cases, 8);
  await same(['show', '@latest'], ['@latest']);
  await same(['tag', 'baseline', '@latest'], ['baseline', '@latest']);
  await same(['tag', 'baseline', '@first'], ['baseline', '@first']);
  await same(['tag', 'bad tag', '@latest'], ['bad tag', '@latest']);
  await same(['show', 'missing'], ['missing']);
  await same(['list'], []);
  await same(['split', '--test-ratio', '0.5', '--seed', '7'], [], { 'test-ratio': '0.5', seed: '7' });
  await same(['split', '--test-ratio', '2', '--seed', '7'], [], { 'test-ratio': '2', seed: '7' });
  await same(['use', 'baseline'], ['baseline']);
  await same(['review', 'baseline'], ['baseline']);
  const cases = (await reference('show', ['baseline'])).cases;
  const generated = cases.find(({ state }) => state === 'generated').id;
  const stale = cases.find(({ state }) => state === 'needs_review').id;
  const review = (ids, state, reviewer, type) => same(['review', 'baseline', ...ids.flatMap((id) => ['--case', id]), '--state', state,
    '--reviewer', reviewer, '--reviewer-type', type], ['baseline'], { case: ids, state, reviewer, 'reviewer-type': type });
  await review([generated], 'reviewed', 'me', 'human');
  await review([generated, stale, generated], 'rejected', 'bot', 'agent');
  await review(['case-x'], 'rejected', 'bot', 'agent');
  await review([generated], 'maybe', 'bot', 'agent');
  await same(['diff', '@first', 'baseline'], ['@first', 'baseline']);

  // A changed document makes its cases need review, and a removed one makes them inactive.
  for (const directory of ['kb', 'ref']) {
    await writeFile(join(root, directory, 'docs', 'guide.md'), documents['guide.md'].replace('６か月', '１２か月'));
    await rm(join(root, directory, 'docs', 'other.md'));
    assert.equal((await run(directory, 'build')).status, 'succeeded');
  }
  const synced = (await same(['sync', 'baseline'], ['baseline'])).actual;
  assert.ok(synced.needs_review > 0 && synced.inactive > 0);

  const exported = [join(root, 'out-kb.jsonl'), join(root, 'out-ref.jsonl')];
  const actualExport = await run('kb', 'eval-data', 'export', 'baseline', '--output', exported[0]);
  const expectedExport = await reference('export', ['baseline'], { output: exported[1] });
  assert.equal(actualIds(JSON.stringify(actualExport.data)), expectedIds(JSON.stringify(expectedExport)));
  assert.equal(actualIds(await readFile(exported[0], 'utf8')), expectedIds(await readFile(exported[1], 'utf8')));
  await same(['export', 'baseline'], ['baseline']);

  // Saved datasets and tags match, and each dataset ID is the digest of its cases.
  const [savedActual, savedExpected] = [rows(actualDb, 'evaluation_datasets'), rows(db, 'evaluation_datasets')];
  const parsed = (saved) => JSON.stringify(saved.map(({ id, data }) => ({ id, data: JSON.parse(data) })));
  assert.equal(actualIds(parsed(savedActual)), expectedIds(parsed(savedExpected)));
  for (const { id, data } of savedActual) assert.equal(id, 'ed-' + sha256(JSON.stringify(JSON.parse(data).cases)));
  assert.equal(actualIds(JSON.stringify(rows(actualDb, 'evaluation_dataset_tags'))), expectedIds(JSON.stringify(rows(db, 'evaluation_dataset_tags'))));
});
