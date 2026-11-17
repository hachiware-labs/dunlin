import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { mkdir, mkdtemp, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import { DatabaseSync } from 'node:sqlite';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { promisify } from 'node:util';

const cli = fileURLToPath(new URL('../cli.mjs', import.meta.url));
const fixture = JSON.parse(await readFile(new URL('./fixtures/layout-before-2e.json', import.meta.url), 'utf8'));
const runNode = promisify(execFile);

// The tables of the layout before step 2e, as the JavaScript implementation created them.
const oldTables = `CREATE TABLE IF NOT EXISTS loaded_files (id TEXT PRIMARY KEY, document TEXT NOT NULL);
  CREATE TABLE IF NOT EXISTS document_chunks (id TEXT PRIMARY KEY, chunks TEXT NOT NULL);
  CREATE TABLE IF NOT EXISTS stage_payloads (id TEXT PRIMARY KEY, data TEXT NOT NULL);`;

/** The parts of a search that the layout must not change: ranks, scores, ranges, text and context. */
function comparable(data, project) {
  const base = pathToFileURL(project).href;
  return {
    results: data.results.map(({ rank, score, document_id, range, text, heading, page, evidence }) =>
      ({ rank, score, document_id, range, text, heading, page, evidence: { range: evidence.range, byteRange: evidence.byteRange } })),
    context: data.context.text.split(base).join('file:///project'),
  };
}

test('moves a project built before step 2e without parsing again and keeps its search results', { timeout: 120_000 }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-layout-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const env = { ...process.env, DUNLIN_GLOBAL_CONFIG: join(root, 'global.yaml') };
  const dunlin = async (...args) => JSON.parse((await runNode(process.execPath, [cli, '--project', 'project', ...args, '--json'],
    { cwd: root, env, encoding: 'utf8', timeout: 60_000 })).stdout);
  await runNode(process.execPath, [cli, 'init', 'mcp', 'project'], { cwd: root, env, encoding: 'utf8', timeout: 60_000 });
  const project = join(root, 'project');
  await writeFile(join(project, 'Dunlinfile.yaml'), fixture.config);
  for (const [name, text] of Object.entries(fixture.files)) {
    await mkdir(dirname(join(project, 'docs', name)), { recursive: true });
    await writeFile(join(project, 'docs', name), text);
  }
  const statePath = join(project, '.dunlin', 'state.sqlite');
  const state = new DatabaseSync(statePath);
  state.exec(oldTables);
  for (const [table, rows] of Object.entries(fixture.tables)) {
    for (const row of rows) {
      const columns = Object.keys(row);
      state.prepare(`INSERT INTO ${table} (${columns.join(', ')}) VALUES (${columns.map(() => '?').join(', ')})`).run(...Object.values(row));
    }
  }
  state.close();

  const built = await dunlin('build');
  assert.deepEqual(built.warnings.map(({ code }) => code), ['DATA_LAYOUT_MIGRATED']);
  assert.equal(built.data.documents, Object.keys(fixture.files).length);
  // Every file came from the moved extraction cache: parsing would have stored its original.
  assert.deepEqual(await readdir(join(project, '.dunlin', 'artifacts', 'originals', 'shared')).catch(() => []), []);
  for (const [query, expected] of Object.entries(fixture.searches)) {
    const { data } = await dunlin('search', query, '--limit', '5');
    assert.deepEqual(comparable(data, project), expected, query);
  }

  const moved = new DatabaseSync(statePath, { readOnly: true });
  try {
    const tables = moved.prepare("SELECT name FROM sqlite_master WHERE type = 'table'").all().map(({ name }) => name);
    assert.deepEqual(tables.filter((name) => ['loaded_files', 'stage_payloads', 'document_chunks'].includes(name)), []);
    assert.equal(moved.prepare('SELECT count(*) AS count FROM extractions').get().count, fixture.tables.loaded_files.length);
    const kept = moved.prepare("SELECT manifest FROM stage_outputs WHERE id = ?").get(fixture.tables.latest_stage.find(({ stage }) => stage === 'source').id);
    const source = JSON.parse(kept.manifest);
    assert.equal(source.migrated, true);
    assert.ok(source.groups[0][1].every((entry) => /^[0-9a-f]{64}$/u.test(entry.version) && !('content' in entry)));
  } finally {
    moved.close();
  }

  const again = await dunlin('build');
  assert.deepEqual(again.warnings, []);
  assert.equal(again.data.build_id, built.data.build_id);
  assert.equal(again.data.stages.source.reused, true);
});
