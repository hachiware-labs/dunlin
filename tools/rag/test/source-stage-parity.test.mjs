import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { DatabaseSync } from 'node:sqlite';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { runSourceStage } from '../project-content.mjs';
import { parserRoutes } from '../project-parsers.mjs';
import { referenceSourceStage as reference } from './fixtures/source-stage-reference.mjs';

// Values cross to Rust as JSON, so -0 comes back as 0. The stored JSON is the same.
const referenceSourceStage = async (...args) => JSON.parse(JSON.stringify(await reference(...args)));

const schema = `CREATE TABLE stage_outputs (stage TEXT NOT NULL, id TEXT NOT NULL, manifest TEXT NOT NULL, PRIMARY KEY (stage, id));
  CREATE TABLE latest_stage (stage TEXT PRIMARY KEY, id TEXT NOT NULL);
  CREATE TABLE source_checks (id INTEGER PRIMARY KEY, source_id TEXT NOT NULL, checked_at TEXT NOT NULL);
  CREATE TABLE loaded_files (id TEXT PRIMARY KEY, document TEXT NOT NULL);
  CREATE TABLE stage_payloads (id TEXT PRIMARY KEY, data TEXT NOT NULL);`;

const files = [['a.md', '# A\n本文 "quoted" \ \u0001\n'], ['b.txt', 'plain\r\ntext'], ['page.html', '<main><h1>Title</h1><p>Body</p></main>'],
  ['sub/c.md', '# C\n'], ['data.bin', 'x'], ['empty.md', ''], ['dup.md', '# A\n本文 "quoted" \ \u0001\n']];

// Numbers and strings whose JSON differs between JavaScript and a naive writer.
const modules = [{ use: 'source.filesystem', version: 1, name: 'source.filesystem', with: { path: './docs', exclude: [] } },
  { use: 'loader.auto', version: 1, name: 'loader.auto', with: { unsupported: 'error', ocr: 'none', x: [0.1 + 0.2, 1e21, 1.5e-7, -0, 2 ** 53 + 2, '日本' + String.fromCharCode(0x2028) + '"' + String.fromCharCode(92)] } }];

async function project(open, root, name) {
  const base = join(root, name);
  await mkdir(base);
  const state = join(base, 'state.sqlite');
  const db = new DatabaseSync(state);
  db.exec(schema);
  open.push(db);
  return { db, paths: { state, artifacts: join(base, 'artifacts') } };
}

const rows = (db) => ({
  outputs: db.prepare('SELECT * FROM stage_outputs ORDER BY stage, id').all(),
  payloads: db.prepare('SELECT * FROM stage_payloads ORDER BY id').all(),
  latest: db.prepare('SELECT * FROM latest_stage').all(),
  checks: db.prepare('SELECT source_id FROM source_checks ORDER BY id').all(),
});

test('records the source stage with the identifiers and rows of the JavaScript implementation', async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-source-stage-'));
  const open = [];
  // Close the databases before removing their directory.
  t.after(() => {
    for (const db of open) db.close();
    return rm(root, { recursive: true, force: true, maxRetries: 5 });
  });
  const docs = join(root, 'docs');
  for (const [name, body] of files) {
    await mkdir(join(docs, name, '..'), { recursive: true });
    await writeFile(join(docs, name), body);
  }
  const sources = [{ id: 'docs', files: files.map(([name]) => ({ file: join(docs, name), objectId: name })) },
    { id: 'more', files: [{ file: join(docs, 'b.txt'), objectId: 'b.txt' }] }, { id: 'none', files: [] }];
  const routes = parserRoutes([{ name: 'loader.auto', module: 'loader.auto', category: 'auto', args: { unsupported: 'error', ocr: 'none' } }]);
  const actual = await project(open, root, 'actual');
  const expected = await project(open, root, 'expected');

  const stopped = { sources, routes, reject: true, skipUnsupported: false, modules };
  assert.deepEqual(await runSourceStage(stopped, actual.paths, root), await referenceSourceStage(expected.db, stopped, expected.paths, root));

  for (const force of [false, false, true]) {
    const request = { sources, routes, reject: true, skipUnsupported: true, force, modules };
    const result = await runSourceStage(request, actual.paths, root);
    const reference = await referenceSourceStage(expected.db, request, expected.paths, root);
    assert.deepEqual(result, reference);
    assert.deepEqual(rows(actual.db), rows(expected.db));
  }
  assert.deepEqual(rows(actual.db).checks.length, 3);
  const changed = { sources: sources.slice(1), routes, reject: false, modules: modules.slice(1) };
  assert.deepEqual(await runSourceStage(changed, actual.paths, root), await referenceSourceStage(expected.db, changed, expected.paths, root));
  assert.deepEqual(rows(actual.db), rows(expected.db));
});
