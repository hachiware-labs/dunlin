import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, unlink, utimes, writeFile } from 'node:fs/promises';
import { DatabaseSync } from 'node:sqlite';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { Dunlin } from 'dunlin';

import { defaultRoutes, parserRoutes } from '../project-parsers.mjs';
import { referenceBuild, referenceTag } from './fixtures/build-command-reference.mjs';

const schema = `CREATE TABLE stage_outputs (stage TEXT NOT NULL, id TEXT NOT NULL, manifest TEXT NOT NULL, PRIMARY KEY (stage, id));
  CREATE TABLE latest_stage (stage TEXT PRIMARY KEY, id TEXT NOT NULL);
  CREATE TABLE stage_payloads (id TEXT PRIMARY KEY, data TEXT NOT NULL);
  CREATE TABLE document_chunks (id TEXT PRIMARY KEY, chunks TEXT NOT NULL);
  CREATE TABLE transform_outputs (key TEXT PRIMARY KEY, output_id TEXT NOT NULL);
  CREATE TABLE loaded_files (id TEXT PRIMARY KEY, document TEXT NOT NULL);
  CREATE TABLE source_checks (id INTEGER PRIMARY KEY, source_id TEXT NOT NULL, checked_at TEXT NOT NULL);
  CREATE TABLE builds (id TEXT PRIMARY KEY, manifest TEXT NOT NULL);
  CREATE TABLE project (id INTEGER PRIMARY KEY CHECK (id = 1), build_id TEXT REFERENCES builds(id));
  INSERT INTO project VALUES (1, NULL);
  CREATE TABLE build_tags (name TEXT PRIMARY KEY, build_id TEXT NOT NULL);
  CREATE TABLE build_search (build_id TEXT PRIMARY KEY, settings TEXT NOT NULL);
  CREATE TABLE tag_search (name TEXT PRIMARY KEY, settings TEXT NOT NULL);
  CREATE TABLE build_config (ref TEXT PRIMARY KEY, config TEXT NOT NULL);
  CREATE TABLE adoption_history (id INTEGER PRIMARY KEY, previous TEXT NOT NULL, adopted TEXT NOT NULL);
  CREATE TABLE serving_generations (id INTEGER PRIMARY KEY, build_id TEXT NOT NULL);
  CREATE TABLE tag_history (id INTEGER PRIMARY KEY, name TEXT NOT NULL, previous TEXT, target TEXT NOT NULL);
  CREATE TABLE build_runs (id TEXT PRIMARY KEY, build_id TEXT NOT NULL, config_id TEXT NOT NULL, created TEXT NOT NULL);
  CREATE TABLE index_outputs (key TEXT PRIMARY KEY, build_id TEXT NOT NULL);
  CREATE TABLE embeddings (id TEXT PRIMARY KEY, data BLOB NOT NULL);`;

const tables = ['stage_outputs', 'stage_payloads', 'latest_stage', 'document_chunks', 'transform_outputs', 'builds', 'project', 'build_tags',
  'build_search', 'tag_search', 'build_config', 'serving_generations', 'tag_history', 'build_runs', 'index_outputs'];

const plain = (routes) => routes.map(({ name, category, match, ocr }) => ({ name, category, ...(match && { match }), ocr }));

/** Timings and indexing times differ between runs. */
const stable = (value) => JSON.parse(JSON.stringify(value, (key, item) => ['elapsed_ms', 'acquiredAtMs'].includes(key) ? 0
  : ['created', 'checked_at'].includes(key) && typeof item === 'string' ? 'T' : item));

function rows(db) {
  return Object.fromEntries(tables.map((table) => [table, db.prepare(`SELECT * FROM ${table} ORDER BY 1, 2`).all()]));
}

/** The first path where two values differ, so a failure stays short. */
function difference(a, b, path = '') {
  if (JSON.stringify(a) === JSON.stringify(b)) return null;
  if (a && b && typeof a === 'object' && typeof b === 'object') {
    for (const key of new Set([...Object.keys(a), ...Object.keys(b)])) {
      const found = difference(a[key], b[key], path + '/' + key);
      if (found) return found;
    }
    return path + ': key order ' + JSON.stringify(Object.keys(a)) + ' vs ' + JSON.stringify(Object.keys(b));
  }
  return path + ': ' + JSON.stringify(a)?.slice(0, 400) + ' vs ' + JSON.stringify(b)?.slice(0, 400);
}

const failed = (error) => ({ status: 'failed', error: { ...(error.code && { code: error.code }), message: error.message,
  ...(error.exitCode && { exitCode: error.exitCode }), ...(error.data && { data: error.data }) } });

const sourceModules = [{ use: 'source.filesystem', version: 1, name: 'source.filesystem', with: { path: 'docs' } }];
const transformModules = [{ use: 'chunk.structure', version: 1, name: 'chunk.structure', with: { max_chars: 40 } }];
const configIndex = { modules: [{ use: 'index.keyword', with: { tokenizer: 'trigram' } }] };
const index = { keyword: { tokenizer: 'trigram' }, construction: [{ use: 'index.keyword', version: 1, name: 'index.keyword', with: { tokenizer: 'trigram' } }],
  search: [{ use: 'retrieval.keyword' }], rerank: [], configuration: { index: configIndex } };
const textOnly = { routes: parserRoutes([{ name: 'loader.text', module: 'loader.text', category: 'text', args: {} }]), reject: true };
const host = { configChanged: () => false, indexTool: () => { throw new Error('No index tool.'); } };
const searchConfigId = (build) => 'search-' + build.build_id + '-' + JSON.stringify(build.search ?? null).length;

/**
 * Plays the same commands on a new project at `root` with one implementation, and returns each
 * step's result and rows. Document URLs carry the project folder, so both plays use one folder.
 */
async function play(root, implementation) {
  await rm(root, { recursive: true, force: true, maxRetries: 5 });
  const data = join(root, '.dunlin');
  await mkdir(join(data, 'artifacts'), { recursive: true });
  await mkdir(join(root, 'docs'), { recursive: true });
  const state = join(data, 'state.sqlite');
  const db = new DatabaseSync(state);
  const steps = [];
  try {
    db.exec('PRAGMA journal_mode = WAL;' + schema);
    const project = { state, artifacts: join(data, 'artifacts'), corpus: join(data, 'corpus'), root };
    // Modification times are part of the documents, so both plays use the same ones.
    let clock = Date.UTC(2026, 0, 1) / 1000;
    const write = async (file, content) => {
      await writeFile(join(root, 'docs', file), content);
      clock += 60;
      await utimes(join(root, 'docs', file), clock, clock);
    };
    const record = (label, result) => {
      steps.push([label, stable({ result, rows: rows(db) })]);
      return result;
    };
    async function build(label, values, { routes = defaultRoutes(), reject = false } = {}) {
      const { id, files } = await Dunlin.scanSource({ path: join(root, 'docs'), name: 'source.filesystem', projectRoot: root });
      files.sort((a, b) => a.objectId.localeCompare(b.objectId));
      const request = {
        command: 'build', ...project, configIndex, index, transform: { modules: transformModules, embedder: null },
        source: { sources: [{ id, files }], routes: plain(routes), reject, commands: [], modules: sourceModules, ocrVersion: 'none', ocrTool: 'lightonocr' },
        ...values,
      };
      const result = implementation === 'reference'
        ? await referenceBuild(request, host).then(({ output, warnings }) => ({ status: 'done', output, warnings }), failed)
        : await Dunlin.runBuild(request, host);
      return record(label, result);
    }
    async function tag(label, values) {
      const request = { state, search: [{ use: 'retrieval.keyword', with: { top_k: 5 } }], configSearch: { modules: [] },
        configModels: { local: { kind: 'embedding' } }, ...values };
      const result = implementation === 'reference'
        ? await referenceTag(db, request, { searchConfigId }).then((output) => ({ status: 'done', output }), failed)
        : await Dunlin.tagBuild(request, { searchConfigId });
      return record(label, result);
    }

    await write('guide.md', '# 点検手順\n電源を止めます。\n## 校正\nCalibration を行います。\n');
    await write('notes.txt', 'Calibration notes.\n');
    await write('Zeta.md', '# Zeta\nzeta\n');
    await write('image.png', 'not an image');
    await build('dry run on a new project', { dryRun: true });
    await build('transform without a source', { command: 'transform', runId: 'run-0' });
    const first = await build('first build', { runId: 'run-1' });
    await build('unchanged dry run', { dryRun: true });
    await build('forced dry run', { dryRun: true, force: true });
    await write('notes.txt', 'Calibration notes, revised.\n');
    await write('added.md', '# Added\n追加の文書です。\n');
    await unlink(join(root, 'docs', 'Zeta.md'));
    await build('changed dry run', { dryRun: true });
    await build('source only', { command: 'source', runId: 'run-2' });
    await build('index from a stale transform', { from: 'index', runId: 'run-3' });
    await build('transform and index', { from: 'transform', runId: 'run-4' });
    await build('index from the first build', { from: 'index', input: first.output.build_id, runId: 'run-5' });
    await build('dry run from an input', { dryRun: true, from: 'transform', input: first.output.build_id });
    await build('missing input', { dryRun: true, from: 'index', input: 'bld-missing' });
    await build('tagged candidate', { from: 'transform', tag: 'candidate', candidate: true, runId: 'run-6' });
    await build('until transform', { until: 'transform', runId: 'run-7' });
    await build('unsupported files', { runId: 'run-8' }, textOnly);
    await build('skipped unsupported files', { runId: 'run-9', skipUnsupported: true }, textOnly);
    await tag('serving build', { name: 'v1' });
    await tag('the same tag again', { name: 'v1' });
    await tag('a build ID', { name: 'first', reference: first.output.build_id });
    await tag('another build without --move', { name: 'v1', reference: first.output.build_id });
    await tag('another build with --move', { name: 'v1', reference: first.output.build_id, move: true });
    await tag('a tag', { name: 'copy', reference: 'candidate' });
    await tag('a missing build', { name: 'none', reference: 'bld-missing' });
  } finally {
    db.close();
  }
  return steps;
}

test('runs build commands with the stages, inputs, records and dry runs of the JavaScript implementation', { timeout: 240_000 }, async (t) => {
  const base = await mkdtemp(join(tmpdir(), 'dunlin-build-command-'));
  t.after(() => rm(base, { recursive: true, force: true, maxRetries: 5 }));
  const root = join(base, 'kb');
  const expected = await play(root, 'reference');
  const actual = await play(root, 'rust');
  assert.deepEqual(actual.map(([label]) => label), expected.map(([label]) => label));
  for (const [i, [label, result]] of actual.entries()) assert.equal(difference(result, expected[i][1]), null, label);

  const step = (label) => actual.find(([name]) => name === label)[1].result;
  assert.deepEqual(step('dry run on a new project').output.files.map(({ path, change, stages }) => [path, change, stages.source]),
    [['guide.md', 'added', 'run'], ['image.png', 'added', 'skip'], ['notes.txt', 'added', 'run'], ['Zeta.md', 'added', 'run']]);
  assert.equal(step('transform without a source').error.code, 'STAGE_REQUIRED');
  const first = step('first build').output;
  assert.equal(first.serving_changed, true);
  assert.deepEqual(Object.keys(first.stages), ['source', 'transform', 'index']);
  assert.deepEqual(step('unchanged dry run').output.files.map(({ stages }) => stages.index), ['reuse', 'skip', 'reuse', 'reuse']);
  assert.deepEqual(step('changed dry run').output.files.map(({ path, change }) => [path, change]),
    [['added.md', 'added'], ['guide.md', 'unchanged'], ['image.png', 'added'], ['notes.txt', 'updated'], ['Zeta.md', 'deleted']]);
  assert.equal(step('index from a stale transform').warnings[0].code, 'STALE_STAGE');
  assert.equal(step('index from the first build').output.build_id, first.build_id);
  assert.equal(step('dry run from an input').output.input_id, first.stages.source.id);
  assert.equal(step('missing input').error.code, 'BUILD_REQUIRED');
  assert.equal(step('tagged candidate').output.serving_changed, false);
  assert.equal(step('unsupported files').error.code, 'UNSUPPORTED_FILES');
  assert.equal(step('another build without --move').error.code, 'TAG_EXISTS');
  assert.equal(step('a missing build').error.code, 'BUILD_REQUIRED');
});
