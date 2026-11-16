import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { createServer } from 'node:http';
import { existsSync } from 'node:fs';
import { mkdir, mkdtemp, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import { DatabaseSync } from 'node:sqlite';
import { tmpdir } from 'node:os';
import { join, relative, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';
import { Dunlin } from 'dunlin';

import { referenceIndexStage } from './fixtures/index-stage-reference.mjs';

const schema = `CREATE TABLE stage_outputs (stage TEXT NOT NULL, id TEXT NOT NULL, manifest TEXT NOT NULL, PRIMARY KEY (stage, id));
  CREATE TABLE latest_stage (stage TEXT PRIMARY KEY, id TEXT NOT NULL);
  CREATE TABLE stage_payloads (id TEXT PRIMARY KEY, data TEXT NOT NULL);
  CREATE TABLE document_chunks (id TEXT PRIMARY KEY, chunks TEXT NOT NULL);
  CREATE TABLE transform_outputs (key TEXT PRIMARY KEY, output_id TEXT NOT NULL);
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

const tables = ['stage_outputs', 'stage_payloads', 'latest_stage', 'document_chunks', 'builds', 'project', 'build_tags', 'build_search',
  'tag_search', 'build_config', 'serving_generations', 'tag_history', 'build_runs', 'index_outputs', 'embeddings'];

const revision = (content) => createHash('sha256').update(content).digest('hex');
const repository = fileURLToPath(new URL('../../../', import.meta.url));
const toolPath = join(repository, 'dist', 'tools', `dunlin-hannoy-${process.platform}-${process.arch}${process.platform === 'win32' ? '.exe' : ''}`);

const manual = '# 点検手順\n電源を止めます。フィルタを外します。\n## 校正\nCalibration ＡＢＣ①. Replace the filter.\n\n点検は毎月です。\n';
const document = (objectId, content, format = 'markdown') => ({ objectId, url: 'file:///' + objectId, format, modifiedAtMs: 1700000000000, content });
const sources = {
  first: [
    ['docs', [document('manual.md', manual), document('notes.txt', 'Calibration notes. Check the sensor.\nThen calibrate again.\n', 'text'),
      document('blank.txt', '   \n', 'text')]],
    ['more', [document('copy.md', manual)]],
  ],
  second: [
    ['docs', [document('manual.md', manual + '\n## 追記\n点検の記録を残します。\n'), document('notes.txt', 'Calibration notes. Check the sensor.\n', 'text')]],
  ],
  third: [
    ['docs', [document('manual.md', manual + '\n## 追記\n点検の記録を残します。\n'), document('extra.md', '# 交換\nフィルタを交換します。\n')]],
    ['new', [document('faq.md', '# FAQ\nCalibration twice a year.\n')]],
  ],
  empty: [['docs', []]],
};

/** Records a source stage as Rust does: contents as payloads. */
function saveSource(db, id, groups) {
  const insert = db.prepare('INSERT OR IGNORE INTO stage_payloads VALUES (?, ?)');
  const stored = groups.map(([source, documents]) => [source, documents.map(({ content, ...rest }) => {
    const json = JSON.stringify(content);
    insert.run(revision(json), json);
    return { ...rest, text: revision(json) };
  })]);
  const files = groups.reduce((sum, [, documents]) => sum + documents.length, 0);
  db.prepare("INSERT INTO stage_outputs VALUES ('source', ?, ?)").run(id, JSON.stringify({
    id, config_id: 'cfg-source', modules: [{ use: 'source.filesystem' }], files, skipped: [], groups: stored, payloads: 1 }));
}

const transformModules = [{ use: 'chunk.structure', version: 1, name: 'chunk.structure', with: { max_chars: 30 } }];

function vector(text) {
  const noise = (text.length % 7) * 0.0123456789012345;
  if (text.includes('点検')) return [1, 0.2 + noise, 1e-7];
  if (/calibrat/iu.test(text)) return [0.1 + noise, 1, 0];
  return [noise, 0.3, 1.0000000000000002];
}

/** Every file under a build directory: path, and the bytes of the files Dunlin writes itself. */
async function files(directory) {
  const out = {};
  const walk = async (dir) => {
    for (const entry of await readdir(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name);
      const name = relative(directory, path).split(sep).join('/');
      // Tantivy names its segment files at random; the keyword index is compared by its searches.
      if (name === '.dunlin-index-v1') continue;
      if (entry.isDirectory()) await walk(path);
      else if (name.endsWith('vectors.json')) out[name] = JSON.parse(await readFile(path, 'utf8'));
      else if (/vectors\.f32$|\.csv$/u.test(name)) out[name] = await readFile(path, name.endsWith('.f32') ? undefined : 'utf8');
      else out[name] = true;
    }
  };
  if (existsSync(directory)) await walk(directory);
  return out;
}

/** Timestamps of indexing and of the build run differ between runs. */
const stable = (value) => JSON.parse(JSON.stringify(value, (key, item) =>
  key === 'acquiredAtMs' ? 0 : key === 'created' && typeof item === 'string' ? 'T' : item));

/** What the keyword index of a build finds, opened as search opens it. */
async function keywordIndex(project, runId, result) {
  const manifest = result.output && project.db.prepare('SELECT manifest FROM builds WHERE id = ?').get(result.output.build_id);
  if (!manifest) return null;
  const build = JSON.parse(manifest.manifest);
  const location = join(project.artifacts, build.index);
  const status = await Dunlin.inspectIndex(location);
  const keyword = build.keyword?.tokenizer === 'lindera'
    ? { keywordJapanese: build.keyword.dictionary_hash ? { userDictionary: join(project.artifacts, 'dictionaries', build.keyword.dictionary_hash + '.csv') } : {} }
    : { keywordNgrams: true, keywordBigrams: build.keyword?.short_query === 'bigram' };
  const database = await Dunlin.open(location, { corpusBackend: 'sqlite', corpusDirectory: project.corpus, ...keyword });
  try {
    const hits = [];
    for (const query of ['点検', 'calibration', 'フィルタ']) hits.push((await database.search(query, { limit: 10 })).hits);
    return { index: build.index === runId, documents: status.searchableDocuments, sources: status.sources, hits };
  } finally { await database.close(); }
}

function rows(db) {
  return stable(Object.fromEntries(tables.map((table) => [table, db.prepare(`SELECT * FROM ${table} ORDER BY 1, 2`).all()
    .map((row) => Object.fromEntries(Object.entries(row).map(([key, value]) => [key, value instanceof Uint8Array ? Buffer.from(value).toString('hex') : value])))])));
}

test('runs the index stage with the build IDs, files and rows of the JavaScript implementation', { timeout: 240_000 }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-index-stage-'));
  const bodies = { reference: [], rust: [] };
  let current = 'reference';
  const server = createServer((request, response) => {
    let body = '';
    request.setEncoding('utf8');
    request.on('data', (part) => { body += part; });
    request.on('end', () => {
      bodies[current].push(body);
      const { input } = JSON.parse(body);
      response.setHeader('content-type', 'application/json');
      response.end(JSON.stringify({ embeddings: input.map(vector) }));
    });
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const base = `http://127.0.0.1:${server.address().port}`;
  const open = [];
  t.after(async () => {
    for (const db of open) db.close();
    await new Promise((resolve) => server.close(resolve));
    await rm(root, { recursive: true, force: true, maxRetries: 5 });
  });

  const projects = {};
  for (const name of ['reference', 'rust']) {
    const projectRoot = join(root, name);
    const data = join(projectRoot, '.dunlin');
    await mkdir(join(data, 'artifacts'), { recursive: true });
    await writeFile(join(projectRoot, 'dict.csv'), '点検手順,カスタム名詞,テンケンテジュン\n');
    await writeFile(join(projectRoot, 'bad.csv'), '選択,名詞\n');
    const state = join(data, 'state.sqlite');
    const db = new DatabaseSync(state);
    open.push(db);
    db.exec(schema);
    projects[name] = { root: projectRoot, db, state, artifacts: join(data, 'artifacts'), corpus: join(data, 'corpus') };
  }

  const embedder = { provider: 'ollama', model: 'embed', url: base + '/api/embed' };
  const keyword = { tokenizer: 'trigram', short_query: 'bigram' };
  const exact = { backend: 'exact', settings: { model: 'local' } };
  const hannoy = { backend: 'hannoy', settings: { model: 'local', ef_construction: 100 } };
  const search = [{ use: 'retrieval.keyword', version: 1, name: 'retrieval.keyword', with: { top_k: 10 } }];
  const tool = existsSync(toolPath);

  let transformId;
  async function stage(sourceName) {
    for (const name of ['reference', 'rust']) {
      const { db, state } = projects[name];
      if (!db.prepare("SELECT 1 FROM stage_outputs WHERE stage = 'source' AND id = ?").get('src-' + sourceName)) saveSource(db, 'src-' + sourceName, sources[sourceName]);
      const result = await Dunlin.runTransform({ state, sourceId: 'src-' + sourceName, force: false, modules: transformModules });
      assert.equal(result.status, 'done');
      transformId = result.output.id;
    }
  }

  async function both(runId, settings, { changed = false } = {}) {
    const results = {};
    for (const name of ['reference', 'rust']) {
      current = name;
      const project = projects[name];
      const request = {
        state: project.state, artifacts: project.artifacts, corpus: project.corpus, root: project.root, runId, transformId,
        keyword, search, rerank: [], construction: [{ use: 'index.keyword' }], configuration: { index: { modules: ['x'] } },
        activate: true, generation: null, ...settings,
        ...(settings.keyword?.user_dictionary && { keyword: { ...settings.keyword, user_dictionary: join(project.root, settings.keyword.user_dictionary) } }),
      };
      const host = { configChanged: () => changed, indexTool: () => toolPath };
      if (name === 'reference') {
        results[name] = await referenceIndexStage(project.db, request, host).then(({ output, warnings }) => ({ status: 'done', output, warnings }),
          (error) => ({ status: 'failed', error: { ...(error.code && { code: error.code }), message: error.message,
            ...(error.exitCode && { exitCode: error.exitCode }), ...(error.data && { data: error.data }) } }));
      } else results[name] = await Dunlin.runIndex(request, host);
      results[name].files = await files(join(project.artifacts, runId));
      results[name].index = await keywordIndex(project, runId, results[name]);
      results[name].dictionaries = await files(join(project.artifacts, 'dictionaries'));
      results[name].rows = rows(project.db);
      // Messages name the project's own files.
      results[name] = JSON.parse(JSON.stringify(results[name]).replaceAll(JSON.stringify(project.root).slice(1, -1), '<root>'));
    }
    assert.deepEqual(stable(results.rust), stable(results.reference), runId);
    assert.deepEqual(bodies.rust, bodies.reference, runId);
    return results.rust;
  }

  await stage('first');
  const first = await both('run-1', { vector: exact, embedder });
  assert.equal(first.status, 'done');
  assert.ok(first.output.vector.count > 0);
  assert.ok(bodies.rust.length > 0);
  assert.equal((await both('run-2', { vector: exact, embedder })).output.reused, true);

  // A changed source updates a copy of the serving build's keyword index.
  await stage('second');
  const second = await both('run-3', { vector: exact, embedder });
  assert.equal(second.output.reused, false);
  assert.equal(second.index.index, true);
  assert.ok(second.index.hits[0].length > 0);

  if (tool) {
    const graph = await both('run-4', { vector: hannoy, embedder, tag: 'graph' });
    assert.ok(graph.files['hannoy/data.mdb']);
    // The next hannoy build updates the serving graph in place.
    await stage('third');
    const updated = await both('run-5', { vector: hannoy, embedder });
    assert.ok(updated.output.vector.reused > 0);
  } else t.diagnostic('hannoy is not built; skipped the hannoy builds');

  // Japanese keywords with a user dictionary, tagged without serving.
  await both('run-6', { keyword: { tokenizer: 'lindera', user_dictionary: 'dict.csv' }, activate: false, tag: 'ja' });
  const invalid = await both('run-7', { keyword: { tokenizer: 'lindera', user_dictionary: 'bad.csv' } });
  assert.equal(invalid.error.code, 'INVALID_USER_DICTIONARY');
  const missing = await both('run-8', { keyword: { tokenizer: 'lindera', user_dictionary: 'none.csv' } });
  assert.equal(missing.error.code, 'USER_DICTIONARY_REQUIRED');

  // A forced build, while the adoption changed: the serving build stays.
  const forced = await both('run-9', { vector: exact, embedder, force: true, generation: 7 });
  assert.equal(forced.warnings[0].code, 'ADOPTION_CHANGED');
  assert.equal((await both('run-10', { vector: exact, embedder }, { changed: true })).warnings.length, 1);

  const conflict = await both('run-11', { tag: 'ja', search: [] });
  assert.equal(conflict.error.code, 'TAG_EXISTS');

  await stage('empty');
  const empty = await both('run-12', {});
  assert.equal(empty.error.code, 'NO_SEARCHABLE_DOCUMENTS');
  assert.deepEqual(empty.files, {});
});
