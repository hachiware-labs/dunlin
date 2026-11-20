import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdtemp, readFile, rename, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { parse, stringify } from 'yaml';
import { chunkCorpus } from './fixtures/cli-reference/project-content.mjs';
import { describeModules } from './fixtures/cli-reference/project-modules.mjs';

const cli = fileURLToPath(new URL('../cli.mjs', import.meta.url));
const exec = promisify(execFile);

test('keeps the chunk.length compatibility fixture stable', () => {
  const version = describeModules('chunk.length').version;
  const expected = { 1: '5e28e4068dd98329af1736e0d093a58ecbf9bbdaf4d6d0b6fd7295b4cff23c0c' };
  const corpus = { unicode: 'nfkc', groups: [['docs', [{ content: '# 作業\nＡＢＣを確認する。\n次の手順です。', format: 'markdown' }]]] };
  const output = chunkCorpus(corpus, { size: 11, overlap: 2 });
  assert.equal(createHash('sha256').update(JSON.stringify(output)).digest('hex'), expected[version]);
});

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-acceptance-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const run = (...args) => exec(process.execPath, [cli, ...args], { cwd: root, env: { ...process.env, DUNLIN_GLOBAL_CONFIG: join(root, 'global.yaml') }, encoding: 'utf8', timeout: 10_000 });
  const invoke = (...args) => run('--project', 'kb', ...args, '--json').then(({ stdout }) => JSON.parse(stdout));
  const json = async (...args) => {
    const result = await invoke(...args);
    assert.equal(result.schema_version, 1);
    assert.equal(result.status, 'succeeded');
    return result.data;
  };
  const failure = async (exitCode, errorCode, ...args) => {
    await assert.rejects(invoke(...args), (error) => {
      assert.equal(error.code, exitCode);
      const result = JSON.parse(error.stdout);
      assert.equal(result.schema_version, 1);
      assert.equal(result.status, 'failed');
      assert.equal(result.errors[0].code, errorCode);
      return true;
    });
  };
  await run('init', 'mcp', 'kb');
  return { root, run, json, failure, docs: join(root, 'kb', 'docs'), path: join(root, 'kb', 'Dunlinfile.yaml') };
}

test('rejects unavailable and ambiguous evidence, preserves confirmed mappings, and isolates held-out cases', { timeout: 40_000 }, async (t) => {
  const { json, docs, failure } = await fixture(t);
  const quote = 'フィルターは6か月ごとに交換する。';
  const guide = join(docs, 'guide.txt');
  await writeFile(guide, quote + '\n');
  await writeFile(join(docs, 'other.md'), '# 前半\n点検は月に1回。\n# 後半\n点検は月に1回。\n');
  await failure(3, 'EVAL_DATA_REQUIRED', 'eval');
  await json('build');
  await failure(2, 'INVALID_EVAL_DATA', 'eval-data', 'add', '--question', '交換', '--doc', 'guide.txt', '--quote', '架空');
  await failure(2, 'INVALID_EVAL_DATA', 'eval-data', 'add', '--question', '点検', '--doc', 'other.md', '--quote', '点検は月に1回。');
  const first = await json('eval-data', 'add', '--question', '交換', '--doc', 'guide.txt', '--quote', quote);
  await json('eval-data', 'add', '--question', '点検', '--doc', 'other.md', '--quote', '点検は月に1回。', '--heading', '後半');
  await json('eval-data', 'use', first.id);
  const before = await json('evidence-map', 'create');
  const { key, range } = Object.values(before.positions).find(({ doc }) => doc.endsWith('guide.txt'));
  await json('evidence-map', 'confirm', before.id, '--key', key, '--start', String(range.start), '--end', String(range.end), '--reviewer', 'human');
  await writeFile(join(docs, 'separate.txt'), '取扱手順を確認する。\n');
  await json('build');
  await json('eval-data', 'add', '--question', '取扱', '--doc', 'separate.txt', '--quote', '取扱手順を確認する。');
  await json('eval-data', 'split', '--test-ratio', '0.5', '--seed', '51');
  const next = await json('evidence-map', 'create');
  assert.equal(next.positions[key].confirmed, true);
  await json('eval', '--tag', 'dev');
  assert.equal((await json('eval', '--split', 'test', '--tag', 'holdout-1')).test_runs, 1);
  assert.equal((await json('eval', '--split', 'test', '--tag', 'holdout-2')).test_runs, 2);
  assert.ok((await json('report', 'list')).every(({ split }) => split === 'dev'));
  await json('eval-data', 'use', first.id);
  await writeFile(guide, 'フィルターは12か月ごとに交換する。\n');
  await json('build');
  const candidate = Object.values((await json('evidence-map', 'create')).positions).find(({ doc }) => doc.endsWith('guide.txt'));
  assert.ok(candidate.suggestions.length > 0);
  assert.equal(candidate.range, undefined);
  const unresolved = await json('eval');
  assert.equal(unresolved.scores['evidence_recall@context'], 0);
  assert.equal(unresolved.diagnostic['evidence_recall@context'], null);
  assert.equal(unresolved.evidence.unresolved, 1);
});

test('keeps completed stages and the serving index through scan and index failures', { timeout: 30_000 }, async (t) => {
  const { json, run, docs, path, failure } = await fixture(t);
  await writeFile(join(docs, 'guide.txt'), '交換手順を確認する。\n');
  const first = await json('build');
  const text = (await run('--project', 'kb', 'status')).stdout;
  assert.match(text.split('\n')[0], /^bld-\S+ search-\S+ unevaluated/u);
  const { storage } = await json('status');
  assert.ok(storage.build_index_logical_bytes > 0);
  // Text files keep no original file: the corpus restores them (spec 28.14).
  assert.equal(storage.shared_originals_logical_bytes, 0);
  assert.ok(storage.shared_corpus_logical_bytes > 0);
  await rename(docs, docs + '-hidden');
  await failure(3, 'SOURCE_MISSING', 'build');
  assert.equal((await json('search', '手順')).build_id, first.build_id);
  await rename(docs + '-hidden', docs);
  await writeFile(join(docs, 'guide.txt'), '別の交換手順です。\n');
  const config = parse(await readFile(path, 'utf8'));
  config.models = { embedding: { provider: 'ollama', kind: 'embedding', model: 'test',
    base_url: 'http://127.0.0.1:1' } };
  config.index.modules.push({ use: 'index.vector', with: { model: 'embedding' } });
  await writeFile(path, stringify(config));
  await failure(1, 'EXECUTION_FAILED', 'build');
  assert.equal((await json('search', '手順')).build_id, first.build_id);
  const captured = (await json('build', '--from', 'index', '--dry-run')).input_id;
  config.index.modules.pop();
  await writeFile(path, stringify(config));
  const built = await json('build', '--from', 'index');
  assert.equal(built.transform_id, captured);
  assert.notEqual(built.build_id, first.build_id);
  const token = 'should-not-leak-7db99d';
  config.models.embedding.base_url = 'https://user:' + token + '@example.invalid/v1?token=' + token;
  await writeFile(path, stringify(config));
  await assert.rejects(run('--project', 'kb', 'validate', '--json'), (error) => {
    assert.equal(error.code, 2);
    assert.ok(!(error.stdout + error.stderr).includes(token));
    return true;
  });
});

test('keeps a shared context budget across chunk sizes and reports the built settings', { timeout: 30_000 }, async (t) => {
  const { json, docs, path, root } = await fixture(t);
  const quote = '部品交換前に停止する。';
  const text = Array.from({ length: 26 }, (_, i) => '交換' + i + ' の作業を確認する。').join('\n') + '\n' + quote;
  await writeFile(join(docs, 'guide.txt'), text);
  const config = parse(await readFile(path, 'utf8'));
  config.search.modules[1].with.top_k = 1;
  config.search.modules[2].with.budget_chars = 3000;
  await writeFile(path, stringify(config));
  const large = await json('build', '--tag', 'large');
  await json('use', 'large');
  await json('eval-data', 'add', '--question', '交換', '--doc', 'guide.txt', '--quote', quote);
  const selected = parse(await readFile(path, 'utf8'));
  selected.eval.modules[0].with.at = ['context', 1];
  await writeFile(path, stringify(selected));
  const baseline = await json('eval', '--tag', 'large-report');
  assert.equal(baseline.scores['evidence_recall@context'], 1);
  selected.transform.modules[1].with = { unit: 'characters', size: 32, overlap: 0 };
  const smallPath = join(root, 'kb', 'small.yaml');
  await writeFile(smallPath, stringify(selected));
  const small = await json('build', '--from', 'transform', '--input', large.build_id, '--config', smallPath, '--tag', 'small');
  assert.equal(small.source_id, large.source_id);
  assert.notEqual(small.build_id, large.build_id);
  const search = await json('search', '交換', '--build', 'small');
  assert.equal(search.results.length, 1);
  assert.ok(search.context.text.includes(quote));
  const trial = await json('eval', '--build', 'small', '--tag', 'small-report');
  assert.equal(trial.budget_chars, baseline.budget_chars);
  assert.equal(trial.search_config_id, search.search_config_id);
  assert.equal(trial.rows[0].context_chars, search.context.chars);
  assert.equal(trial.scores['evidence_recall@context'], 1);
  const report = (await json('report', 'list'))[0];
  const column = 'transform.1.with.size';
  assert.ok(report.columns.includes(column));
  assert.equal(report.results.find(({ id }) => id === trial.id).settings[column], 32);
  await json('tag', 'small', 'large', '--move');
  assert.equal((await json('report', 'show', 'small-report')).build_id, small.build_id);
});
