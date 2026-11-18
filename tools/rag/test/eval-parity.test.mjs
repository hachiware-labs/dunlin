// Spec 28.11 step 4a: the Rust evaluation scores, positions and maps evidence like the JavaScript
// code it replaced (test/fixtures/eval-reference.mjs). Two copies of one built project run the same
// steps, one through the CLI and one through the reference, and their saved records must match.
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { cp, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { DatabaseSync } from 'node:sqlite';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { parse, stringify } from 'yaml';

import { constructionStages, moduleSettings, restoreModules, validateRag } from '../project-modules.mjs';
import { referenceEvaluate, referenceEvidenceMaps } from './fixtures/eval-reference.mjs';
import { referenceOpen } from './fixtures/search-reference.mjs';

const cli = fileURLToPath(new URL('../cli.mjs', import.meta.url));
const runNode = promisify(execFile);

const documents = {
  'guide.md': '# 手順\n\nポンプの交換手順です。\n\n## 点検\n\n月に一度ポンプを点検します。\nフィルターは６か月ごとに\n交換する。\n\n'
    + '## 清掃\n\n月に一度ポンプを点検します。\n吸い込み口を   水で洗います。\n',
  'notes.txt': '保守の記録\n2024年4月にポンプの点検を行った。\n異音があったため、ファンを交換した。\n次回の点検は半年後とする。\n',
  'long.md': '# 記録用紙\n\n' + Array.from({ length: 30 }, (_, i) => `第${i + 1}節 記録用紙の確認と保管。`).join('\n') + '\n',
};

const evidence = (doc, quote, hints = {}) => ({ doc: 'docs:' + doc, quote, ...hints });
const item = (id, question, items, split = 'dev', state = 'provided', group = items[0].doc) => ({
  id: 'case-' + id.padStart(64, '0'), question, evidence: items, group, split, state, input_by: 'provided', created_at: '2026-01-01T00:00:00.000Z' });

const cases = [
  item('1', 'ポンプの交換', [evidence('guide.md', 'ポンプの交換手順です。')]),
  item('2', 'フィルターの交換', [evidence('guide.md', 'フィルターは 6か月ごとに 交換する。')]),
  item('3', '点検の頻度', [evidence('guide.md', '月に一度ポンプを点検します。')]),
  item('4', '清掃の点検', [evidence('guide.md', '月に一度ポンプを点検します。', { heading: '清掃' })]),
  item('5', '点検の前', [evidence('guide.md', '月に一度ポンプを点検します。', { prefix: '## 点検' })]),
  item('6', '点検の後', [evidence('guide.md', '月に一度ポンプを点検します。', { suffix: 'フィルター' })]),
  item('7', '二度の点検', [evidence('guide.md', '月に二度ポンプを点検します。')]),
  item('8', '消えた文書', [evidence('missing.md', '何か')]),
  item('9', 'ページ', [evidence('notes.txt', 'ファンを交換した。', { page: 1 })]),
  item('10', '異音と吸い込み口', [evidence('notes.txt', '異音があったため、ファンを交換した。'), evidence('guide.md', '吸い込み口を 水で洗います。')], 'dev', 'reviewed', 'g2'),
  item('11', '記録用紙', [evidence('long.md', '第17節 記録用紙の確認と保管。')]),
  item('12', '空', [evidence('guide.md', '　')]),
  item('13', '次回', [evidence('notes.txt', '次回の点検は半年後とする。')], 'test'),
  item('14', '交換手順', [evidence('guide.md', 'ポンプの交換手順です。')], 'test'),
  item('15', '生成', [evidence('notes.txt', '保守の記録')], 'dev', 'generated'),
  item('16', '不採用', [evidence('notes.txt', '保守の記録')], 'dev', 'rejected'),
];

/** A built project and a copy of it, with evaluation data in both. */
async function projects(t) {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-eval-parity-'));
  const env = { ...process.env, DUNLIN_GLOBAL_CONFIG: join(root, 'global.yaml') };
  await runNode(process.execPath, [cli, 'init', 'rag', 'kb', '--no-embedding'], { cwd: root, env });
  const base = join(root, 'kb');
  for (const [name, content] of Object.entries(documents)) await writeFile(join(base, 'docs', name), content);
  const configPath = join(base, 'Dunlinfile.yaml');
  const config = parse(await readFile(configPath, 'utf8'));
  config.transform.modules[1].with = { ...config.transform.modules[1].with, size: 80, overlap: 0 };
  config.search.modules.find(({ use }) => use === 'context.assemble').with.budget_chars = 200;
  config['eval-data'] = { use: 'eval-data:parity' };
  config.eval = { split: 'dev', modules: [
    { use: 'metric.evidence_recall', with: { at: [1, 'context', 3], short_evidence_chars: 12, min_coverage: 0.8, unresolved: 'count_as_missed' } },
    { use: 'metric.latency', with: { percentiles: [95, 50, 99] } },
  ] };
  await writeFile(configPath, stringify(config));
  await runNode(process.execPath, [cli, '--project', 'kb', 'build'], { cwd: root, env });
  const state = new DatabaseSync(join(base, '.dunlin', 'state.sqlite'));
  state.exec('CREATE TABLE IF NOT EXISTS evaluation_datasets (id TEXT PRIMARY KEY, data TEXT NOT NULL);'
    + 'CREATE TABLE IF NOT EXISTS evaluation_reports (id TEXT PRIMARY KEY, tag TEXT UNIQUE, data TEXT NOT NULL);'
    + 'CREATE TABLE IF NOT EXISTS evaluation_dataset_tags (name TEXT PRIMARY KEY, dataset_id TEXT NOT NULL);'
    + 'CREATE TABLE IF NOT EXISTS evidence_positions (id TEXT PRIMARY KEY, data TEXT NOT NULL);'
    + 'CREATE TABLE IF NOT EXISTS evidence_maps (id TEXT PRIMARY KEY, data TEXT NOT NULL);'
    + 'CREATE TABLE IF NOT EXISTS evidence_overrides (id TEXT PRIMARY KEY, data TEXT NOT NULL);');
  state.prepare('INSERT INTO evaluation_datasets VALUES (?, ?)').run('eval-data-parity', JSON.stringify({ id: 'eval-data-parity', cases }));
  state.prepare('INSERT INTO evaluation_dataset_tags VALUES (?, ?)').run('parity', 'eval-data-parity');
  state.close();
  await cp(base, join(root, 'ref'), { recursive: true, preserveTimestamps: true });

  const run = (...args) => runNode(process.execPath, [cli, '--project', 'kb', ...args, '--json'], { cwd: root, env, encoding: 'utf8', timeout: 60_000 })
    .then(({ stdout }) => JSON.parse(stdout), (error) => ({ exitCode: error.code, ...JSON.parse(error.stdout) }));

  const refBase = join(root, 'ref');
  const paths = { state: join(refBase, '.dunlin', 'state.sqlite'), artifacts: join(refBase, '.dunlin', 'artifacts'), corpus: join(refBase, '.dunlin', 'corpus') };
  const db = new DatabaseSync(paths.state);
  db.exec('PRAGMA busy_timeout = 500');
  t.after(() => db.close());
  const build = JSON.parse(db.prepare('SELECT manifest FROM project JOIN builds ON builds.id = project.build_id').get().manifest);
  const database = await referenceOpen(paths, build);
  t.after(() => database.close());
  const checked = validateRag(config, configPath, null);
  const plan = build.search ? restoreModules(build.search) : checked.plan.search;
  const stored = JSON.parse(db.prepare('SELECT config FROM build_config WHERE ref = ?').get(build.build_id)?.config ?? '{}');
  const settings = { ...checked.evalSettings, contextBudget: plan.find(({ module }) => module.role === 'context').args.budget_chars,
    construction: Object.fromEntries(constructionStages.map((stage) => [stage, build.construction?.[stage] ?? stored[stage]])),
    searchModules: moduleSettings(plan) };
  const project = { config, evalSettings: checked.evalSettings };
  const search = (query) => database.searchProject({ plan: moduleSettings(plan), query, build, directory: join(paths.artifacts, build.index),
    embedders: {}, rerankModels: [], rerankers: {} }).then(({ output, context, contextStop }) => ({ search_config_id: output.search_config_id,
    hits: output.results.map(({ evidence: found }) => ({ evidence: found })), context: context.map((found) => ({ evidence: found })),
    context_chars: output.context.chars, context_stop: contextStop }));
  const reference = {
    evaluate: (values) => referenceEvaluate(db, project, build, settings, values, 'run-reference', database, search),
    maps: (action, ref, values = {}) => referenceEvidenceMaps(db, project, action, ref, values, database, build),
  };
  const actual = new DatabaseSync(join(base, '.dunlin', 'state.sqlite'), { readOnly: true });
  t.after(() => actual.close());
  return { root, run, reference, db, actual };
}

/** A report without the values that change on every run. */
function stable(report) {
  const { id, latency_ms: latency, rows, ...rest } = report;
  return { ...rest, latency_ms: Object.keys(latency), rows: rows.map(({ latency_ms: ms, ...row }) => row) };
}

const rows = (db, table) => db.prepare(`SELECT id, data FROM ${table} ORDER BY id`).all().map(({ id, data }) => ({ id, data }));
const at = (text) => text.replace(/"at":"[^"]+"/gu, '"at":"time"');

test('scores, positions and maps evidence like the former JavaScript evaluation', { timeout: 180_000 }, async (t) => {
  const { root, run, reference, db, actual } = await projects(t);
  // Registered after the databases close, which Windows needs before removing their files.
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));

  const compareReports = async (args, values) => {
    const result = await run('eval', ...args);
    assert.equal(result.status, 'succeeded', JSON.stringify(result.errors));
    const expected = await reference.evaluate(values);
    assert.equal(JSON.stringify(stable(result.data)), JSON.stringify(stable(expected)));
    const saved = actual.prepare('SELECT data FROM evaluation_reports WHERE id = ?').get(result.data.id).data;
    assert.equal(JSON.stringify(stable(JSON.parse(saved))), JSON.stringify(stable(JSON.parse(db.prepare('SELECT data FROM evaluation_reports WHERE id = ?').get('run-reference').data))));
    db.prepare("UPDATE evaluation_reports SET id = ? WHERE id = 'run-reference'").run('reference-' + Math.random());
    for (const table of ['evidence_positions', 'evidence_maps']) assert.deepEqual(rows(actual, table), rows(db, table), table);
    return result.data;
  };

  const first = await compareReports(['--tag', 'dev1'], { tag: 'dev1' });
  const reasons = new Set(first.rows.flatMap(({ unresolved }) => unresolved));
  assert.deepEqual([...reasons].sort(), ['ambiguous_quote', 'empty_quote', 'quote_not_found', 'source_missing']);
  // The heading, prefix and suffix hints pick one of the repeated sentences.
  for (const id of ['4', '5', '6']) assert.equal(first.rows.find((row) => row.id.endsWith('0' + id)).complete, true, id);
  assert.ok(first.rows.some(({ scores }) => scores['evidence_recall@1'] === 1));
  assert.ok(first.rows.some(({ scores }) => scores['evidence_recall@context'] === 0 && scores['evidence_recall@3'] === 0));
  assert.deepEqual(first.latency_ms && Object.keys(first.latency_ms), ['50', '95', '99']);
  await compareReports(['--split', 'test'], { split: 'test' });
  await compareReports(['--split', 'test'], { split: 'test' });

  const duplicate = await run('eval', '--tag', 'dev1');
  await assert.rejects(reference.evaluate({ tag: 'dev1' }), (error) => {
    assert.equal(duplicate.errors[0].code, error.code);
    assert.equal(duplicate.errors[0].message, error.message);
    assert.equal(duplicate.exitCode, error.exitCode);
    return true;
  });

  const created = await run('evidence-map', 'create');
  assert.equal(created.status, 'succeeded', JSON.stringify(created.errors));
  assert.equal(JSON.stringify(created.data), JSON.stringify(await reference.maps('create')));
  const ambiguous = Object.values(created.data.positions).find(({ reason }) => reason === 'ambiguous_quote');
  const { start, end } = ambiguous.candidates[1];
  const confirmValues = { key: ambiguous.key, start: String(start), end: String(end), reviewer: 'human' };
  const confirmed = await run('evidence-map', 'confirm', created.data.id, '--key', ambiguous.key, '--start', String(start), '--end', String(end), '--reviewer', 'human');
  assert.equal(confirmed.status, 'succeeded', JSON.stringify(confirmed.errors));
  const expectedConfirm = await reference.maps('confirm', created.data.id, confirmValues);
  const { id: _a, ...confirmedRest } = confirmed.data;
  const { id: _b, ...expectedRest } = expectedConfirm;
  assert.equal(at(JSON.stringify(confirmedRest)), at(JSON.stringify(expectedRest)));
  assert.deepEqual(rows(actual, 'evidence_overrides').map(({ id, data }) => ({ id, data: at(data) })),
    rows(db, 'evidence_overrides').map(({ id, data }) => ({ id, data: at(data) })));

  // Both projects continue from the same records.
  const sync = (table) => {
    db.exec(`DELETE FROM ${table}`);
    for (const row of actual.prepare(`SELECT * FROM ${table}`).all()) db.prepare(`INSERT INTO ${table} VALUES (?, ?)`).run(row.id, row.data);
  };
  sync('evidence_overrides');
  sync('evidence_maps');

  const exported = join(root, 'map.json');
  const notFound = Object.values(confirmed.data.positions).find(({ reason }) => reason === 'quote_not_found' && reason);
  assert.ok(notFound.suggestions.length > 0);
  const edited = structuredClone(confirmed.data);
  edited.positions[notFound.key].range = { start: notFound.suggestions[0].start, end: notFound.suggestions[0].end };
  await mkdir(join(root, 'maps'));
  await writeFile(exported, JSON.stringify(edited));
  const imported = await run('evidence-map', 'import', exported);
  assert.equal(imported.status, 'succeeded', JSON.stringify(imported.errors));
  const expectedImport = await reference.maps('import', exported);
  assert.equal(at(JSON.stringify({ ...imported.data, id: '' })), at(JSON.stringify({ ...expectedImport, id: '' })));
  sync('evidence_overrides');
  sync('evidence_maps');

  for (const [args, values] of [
    [['confirm', confirmed.data.id, '--key', 'pos-missing', '--start', '0', '--end', '1', '--reviewer', 'x'], ['confirm', confirmed.data.id, { key: 'pos-missing', start: '0', end: '1', reviewer: 'x' }]],
    // Byte 3 is inside 手.
    [['confirm', confirmed.data.id, '--key', ambiguous.key, '--start', '3', '--end', '4', '--reviewer', 'x'], ['confirm', confirmed.data.id, { key: ambiguous.key, start: '3', end: '4', reviewer: 'x' }]],
    [['confirm', confirmed.data.id, '--key', ambiguous.key, '--start', 'a', '--end', '2', '--reviewer', 'x'], ['confirm', confirmed.data.id, { key: ambiguous.key, start: 'a', end: '2', reviewer: 'x' }]],
    [['confirm', 'map-missing', '--key', ambiguous.key, '--start', '0', '--end', '3', '--reviewer', 'x'], ['confirm', 'map-missing', { key: ambiguous.key, start: '0', end: '3', reviewer: 'x' }]],
    [['import', join(root, 'none.json')], ['import', join(root, 'none.json')]],
    [['show', 'map-missing'], ['show', 'map-missing']],
  ]) {
    const result = await run('evidence-map', ...args);
    await assert.rejects(reference.maps(...values), (error) => {
      assert.equal(result.errors[0].code, error.code, args.join(' '));
      assert.equal(result.errors[0].message, error.message, args.join(' '));
      assert.equal(result.exitCode, error.exitCode, args.join(' '));
      return true;
    }, args.join(' '));
  }

  const list = await run('evidence-map', 'list');
  assert.equal(JSON.stringify(list.data), JSON.stringify(await reference.maps('list')));
  const shown = await run('evidence-map', 'show', imported.data.id);
  assert.equal(JSON.stringify(shown.data), JSON.stringify(await reference.maps('show', imported.data.id)));
  const output = join(root, 'maps', 'out.json');
  const written = await run('evidence-map', 'export', imported.data.id, '--output', output);
  assert.deepEqual(written.data, { id: imported.data.id });
  const referenceOutput = join(root, 'maps', 'reference.json');
  await reference.maps('export', imported.data.id, { output: referenceOutput });
  assert.equal(await readFile(output, 'utf8'), await readFile(referenceOutput, 'utf8'));

  // Confirmed and saved positions are used again.
  const again = await compareReports([], {});
  assert.ok(again.rows.some(({ unresolved }) => unresolved.length === 0));
});
