// Spec 28.11 step 4b: report show, list and compare, the evaluation status and the missed evidence
// of dictionary suggestions come from Rust and match the JavaScript code they replaced
// (test/fixtures/report-reference.mjs). Both read the same project.
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { DatabaseSync } from 'node:sqlite';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { parse, stringify } from 'yaml';

import { Dunlin } from 'dunlin';
import { validateRag } from '../project-modules.mjs';
import { referenceDictionaryEvaluation, referenceDictionaryMisses, referenceEvaluationStatus, referenceReports } from './fixtures/report-reference.mjs';
import { referenceOpen } from './fixtures/search-reference.mjs';

const cli = fileURLToPath(new URL('../cli.mjs', import.meta.url));
const runNode = promisify(execFile);

const documents = {
  'guide.md': '# 手順\n\nポンプの交換手順です。\n\n## 点検\n\n月に一度ポンプを点検します。\nフィルターは６か月ごとに交換する。\n\n'
    + '## 清掃\n\n吸い込み口を水で洗います。\n',
  'notes.txt': '保守の記録\n2024年4月にポンプの点検を行った。\n異音があったため、ファンを交換した。\n次回の点検は半年後とする。\n',
  'long.md': '# 記録用紙\n\n' + Array.from({ length: 30 }, (_, i) => `第${i + 1}節 記録用紙の確認と保管。`).join('\n') + '\n',
};

const evidence = (doc, quote, hints = {}) => ({ doc: 'docs:' + doc, quote, ...hints });
let next = 0;
const item = (question, items, split = 'dev', state = 'provided') => {
  next++;
  return { id: 'case-' + String(next).padStart(64, '0'), question, evidence: items, group: 'g' + next, split, state,
    input_by: 'provided', created_at: '2026-01-01T00:00:00.000Z' };
};

const cases = [
  item('ポンプの交換', [evidence('guide.md', 'ポンプの交換手順です。')]),
  item('フィルターの交換', [evidence('guide.md', 'フィルターは6か月ごとに交換する。')]),
  item('清掃', [evidence('guide.md', '吸い込み口を水で洗います。', { heading: '清掃' })]),
  item('異音', [evidence('notes.txt', '異音があったため、ファンを交換した。'), evidence('long.md', '第3節 記録用紙の確認と保管。')]),
  item('記録用紙', [evidence('long.md', '第17節 記録用紙の確認と保管。')]),
  item('第29節', [evidence('long.md', '第29節 記録用紙の確認と保管。')]),
  item('二度の点検', [evidence('guide.md', '月に二度ポンプを点検します。')]),
  item('消えた文書', [evidence('missing.md', '何か')]),
  item('ページ', [evidence('notes.txt', 'ファンを交換した。', { page: 1 })]),
  item('保守', [evidence('notes.txt', '保守の記録')]),
  item('次回', [evidence('notes.txt', '次回の点検は半年後とする。')], 'test'),
  item('交換手順', [evidence('guide.md', 'ポンプの交換手順です。')], 'test'),
];

test('reads reports, statuses and misses like the former JavaScript code', { timeout: 180_000 }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-report-parity-'));
  const env = { ...process.env, DUNLIN_GLOBAL_CONFIG: join(root, 'global.yaml') };
  const run = (...args) => runNode(process.execPath, [cli, '--project', 'kb', ...args, '--json'], { cwd: root, env, encoding: 'utf8', timeout: 60_000 })
    .then(({ stdout }) => JSON.parse(stdout), (error) => ({ exitCode: error.code, ...JSON.parse(error.stdout) }));
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
    { use: 'metric.latency', with: { percentiles: [50, 95] } },
  ] };
  await writeFile(configPath, stringify(config));
  assert.equal((await run('build')).status, 'succeeded');
  const statePath = join(base, '.dunlin', 'state.sqlite');
  const state = new DatabaseSync(statePath);
  state.exec('CREATE TABLE IF NOT EXISTS evaluation_datasets (id TEXT PRIMARY KEY, data TEXT NOT NULL);'
    + 'CREATE TABLE IF NOT EXISTS evaluation_reports (id TEXT PRIMARY KEY, tag TEXT UNIQUE, data TEXT NOT NULL);'
    + 'CREATE TABLE IF NOT EXISTS evaluation_dataset_tags (name TEXT PRIMARY KEY, dataset_id TEXT NOT NULL);'
    + 'CREATE TABLE IF NOT EXISTS evidence_positions (id TEXT PRIMARY KEY, data TEXT NOT NULL);'
    + 'CREATE TABLE IF NOT EXISTS evidence_maps (id TEXT PRIMARY KEY, data TEXT NOT NULL);'
    + 'CREATE TABLE IF NOT EXISTS evidence_overrides (id TEXT PRIMARY KEY, data TEXT NOT NULL);');
  state.prepare('INSERT INTO evaluation_datasets VALUES (?, ?)').run('eval-data-parity', JSON.stringify({ id: 'eval-data-parity', cases }));
  state.prepare('INSERT INTO evaluation_dataset_tags VALUES (?, ?)').run('parity', 'eval-data-parity');
  state.close();

  const evaluate = async (...args) => {
    const result = await run('eval', ...args);
    assert.equal(result.status, 'succeeded', JSON.stringify(result.errors));
    return result.data;
  };
  const a = await evaluate('--tag', 'a');
  await evaluate('--split', 'test', '--tag', 't1');
  const candidate = structuredClone(config);
  candidate.search.modules.find(({ use }) => use === 'context.assemble').with.budget_chars = 90;
  const candidatePath = join(base, 'candidate.yaml');
  await writeFile(candidatePath, stringify(candidate));
  await evaluate('--config', candidatePath, '--tag', 'budget');
  config.transform.modules[1].with.size = 40;
  await writeFile(configPath, stringify(config));
  assert.equal((await run('build')).status, 'succeeded');
  const b = await evaluate('--tag', 'b');
  await evaluate('--split', 'test', '--tag', 't2');

  const db = new DatabaseSync(statePath);
  // A report from before evidence rows were saved per case.
  const legacy = { ...a, id: 'legacy-report', tag: 'legacy', rows: a.rows.map(({ evidence: _, ...row }) => row) };
  db.prepare('INSERT INTO evaluation_reports VALUES (?, ?, ?)').run(legacy.id, legacy.tag, JSON.stringify(legacy));
  const paths = { state: statePath, artifacts: join(base, '.dunlin', 'artifacts'), corpus: join(base, '.dunlin', 'corpus') };
  const builds = db.prepare('SELECT id, manifest FROM builds').all().map(({ manifest }) => JSON.parse(manifest));
  const databases = new Map();
  for (const build of builds) databases.set(build.build_id, await referenceOpen(paths, build));
  t.after(async () => {
    for (const database of databases.values()) await database.close();
    db.close();
  });
  // After the databases close, which Windows needs before removing their files.
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));

  const same = (actual, expected, label) => assert.equal(JSON.stringify(actual), JSON.stringify(expected), label);
  const failsLike = async (result, call, label) => {
    let expected;
    try {
      await call();
    } catch (error) {
      expected = error;
    }
    assert.ok(expected, label);
    assert.equal(result.errors[0].code, expected.code, label);
    assert.equal(result.errors[0].message, expected.message, label);
    assert.equal(result.exitCode, expected.exitCode, label);
  };

  for (const args of [['show', 'a'], ['show', b.id], ['show', 'legacy'], ['list'], ['list', '--split', 'test'],
    ['compare', 'a', 'b'], ['compare', 'b', 'a'], ['compare', 'a', 'budget'], ['compare', 't1', 't2'], ['compare', 'a', 't1'],
    ['compare', 'a', 'legacy'], ['compare', 'a', 'a']]) {
    const split = args.includes('--split') ? args.at(-1) : undefined;
    const refs = args.slice(1).filter((value) => value !== '--split' && value !== split);
    const result = await run('report', ...args);
    assert.equal(result.status, 'succeeded', args.join(' ') + JSON.stringify(result.errors));
    same(result.data, referenceReports(db, args[0], refs, split), args.join(' '));
  }
  const compared = (await run('report', 'compare', 'a', 'b')).data;
  assert.ok(compared.comparable && Object.values(compared.scores).every(({ interval_95: interval }) => interval?.length === 2));
  assert.ok((await run('report', 'list')).data[0].columns.length > 1);
  await failsLike(await run('report', 'show', 'missing'), () => referenceReports(db, 'show', ['missing']), 'show missing');
  await failsLike(await run('report', 'compare', 'a', 'missing'), () => referenceReports(db, 'compare', ['a', 'missing']), 'compare missing');

  const { evalSettings } = validateRag(config, configPath, null);
  const project = { config, evalSettings };
  for (const [buildId, searchId, settings, dataset] of [
    [a.build_id, a.search_config_id, evalSettings, config['eval-data'].use],
    [b.build_id, b.search_config_id, evalSettings, config['eval-data'].use],
    [a.build_id, 'search-other', evalSettings, config['eval-data'].use],
    [a.build_id, a.search_config_id, null, config['eval-data'].use],
    [a.build_id, a.search_config_id, evalSettings, 'eval-data:none'],
    [a.build_id, a.search_config_id, { ...evalSettings, percentiles: [50] }, config['eval-data'].use],
  ]) {
    const actual = Dunlin.evaluationStatus({ state: statePath, buildId, searchId, dataset, settings });
    assert.equal(actual.status, 'done');
    same(actual.output, referenceEvaluationStatus(db, buildId, searchId, { config: { ...config, 'eval-data': { use: dataset } }, evalSettings: settings }),
      buildId + ' ' + searchId);
  }

  for (const reference of ['a', 'b', 'legacy', undefined]) {
    const actual = Dunlin.dictionaryEvaluation({ state: statePath, report: reference, dataset: config['eval-data'].use });
    const expected = referenceDictionaryEvaluation(db, project, reference);
    same(actual.output, expected, String(reference));
    const database = databases.get(expected.report.build_id);
    const misses = await database.dictionaryMisses({ state: statePath, report: actual.output.report, data: actual.output.data });
    const found = [];
    for await (const miss of referenceDictionaryMisses(db, expected.report, expected.data, database)) found.push(miss);
    same(misses.output, found, 'misses ' + reference);
    if (reference !== 'legacy') assert.ok(found.some(({ text: quoted }) => quoted), 'misses with text');
  }
  for (const reference of ['t1', 'missing']) {
    const actual = Dunlin.dictionaryEvaluation({ state: statePath, report: reference, dataset: config['eval-data'].use });
    assert.equal(actual.status, 'failed');
    await failsLike({ errors: [actual.error], exitCode: actual.error.exitCode }, () => referenceDictionaryEvaluation(db, project, reference), reference);
  }
});
