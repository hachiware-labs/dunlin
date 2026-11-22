import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import { DatabaseSync } from 'node:sqlite';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import { loadCorpus } from './fixtures/cli-reference/project-content.mjs';
import { defaultRoutes, parserRoutes } from './fixtures/cli-reference/project-parsers.mjs';
import { loadCorpus as referenceLoad } from './fixtures/reference-load/project-content.mjs';

const fixture = fileURLToPath(new URL('./fixtures/parser.mjs', import.meta.url));

// The reference keeps whole documents in loaded_files; Rust keeps corpus versions in extractions
// (spec 28.3). Both use the same cache keys and return the same documents, apart from `version`.
async function run(load, root, name, sources, routes, rounds = 2) {
  const reference = load === referenceLoad;
  const artifacts = join(root, name, 'artifacts');
  await mkdir(artifacts, { recursive: true });
  const state = join(root, name, 'state.sqlite');
  const db = new DatabaseSync(state);
  if (reference) db.exec('CREATE TABLE IF NOT EXISTS loaded_files (id TEXT PRIMARY KEY, document TEXT NOT NULL)');
  const outputs = [];
  try {
    for (let round = 0; round < rounds; round++) {
      const warnings = [];
      const progress = [];
      const groups = await load(sources, { artifacts, state, corpus: join(root, name, 'corpus') }, [], warnings, db, false, routes,
        (n) => progress.push(n), root)
        .then((groups) => groups.map(([id, documents]) => [id, documents.map(({ version, ...document }) => {
          if (!reference) assert.match(version, /^[0-9a-f]{64}$/u);
          return document;
        })]))
        .catch((error) => ({ error: error.message }));
      outputs.push(JSON.stringify({ groups, warnings, completed: Math.max(0, ...progress) }));
    }
    const rows = db.prepare(reference ? 'SELECT id FROM loaded_files ORDER BY id' : 'SELECT key AS id FROM extractions ORDER BY key').all()
      .map(({ id }) => id);
    const shared = join(artifacts, 'originals', 'shared');
    const names = (await readdir(shared).catch(() => [])).sort();
    const blobs = await Promise.all(names.map((file) => readFile(join(shared, file), 'base64')));
    return { outputs, rows: JSON.stringify(rows), originals: JSON.stringify([names, blobs]) };
  } finally {
    db.close();
  }
}

async function compare(t, files, routes = defaultRoutes(), options = {}) {
  const root = await mkdtemp(join(tmpdir(), 'knowledge-nest-load-parity-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const docs = join(root, 'docs');
  await mkdir(join(docs, 'sub'), { recursive: true });
  for (const [name, body] of files) await writeFile(join(docs, name), body);
  const sources = [{ id: 'docs', files: files.map(([name]) => ({ file: join(docs, name), objectId: name })) }];
  const expected = await run(referenceLoad, root, 'old', sources, routes, options.rounds);
  const actual = await run(loadCorpus, root, 'new', sources, routes, options.rounds);
  assert.deepEqual(actual, expected);
  return actual;
}

test('loads text, HTML, duplicates and external commands like the JavaScript loader', async (t) => {
  const routes = parserRoutes([
    { name: 'loader.command', category: 'command', args: { match: ['*.vsdx'], command: [process.execPath, fixture, '{input}'], input: 'file',
      output: 'json', env: { PARSER_MODE: 'json' }, version: '1', timeout_ms: 20000, concurrency: 2 } },
    { name: 'loader.auto', category: 'auto', args: { ocr: 'none' } },
  ]);
  const result = await compare(t, [
    ['a.md', '# 見出し\n本文です。\n'], ['b.txt', 'plain\r\nCRLF\r\n'], ['bom.md', '﻿# BOM\n本文\n'], ['empty.txt', ''],
    ['page.html', '<html><body><main><h1>題</h1><p>段落</p></main></body></html>'], ['sub/big.md', ('ポンプの点検。\n').repeat(30000)],
    ['same1.txt', 'same'], ['same2.txt', 'same'], ['draw.vsdx', 'ポンプ図面\n'], ['名前 #1.md', '空白と記号\n'],
    ...Array.from({ length: 40 }, (_, i) => [`n${String(i).padStart(2, '0')}.md`, `文書 ${i % 7}\n`]),
  ], routes);
  assert.match(result.outputs[0], /"warnings":\[\{"code":"EMPTY_EXTRACTION"/u);
});

test('reports read, decode and size failures like the JavaScript loader', async (t) => {
  for (const files of [
    [['ok.md', '# A\n'], ['bad.txt', Buffer.from([0xff, 0xfe])]],
    [['big.md', 'x'.repeat(1024 * 1024 + 1)]],
    [['ok.md', '# A\n'], ['fails.vsdx', 'x']],
  ]) {
    const routes = parserRoutes([
      { name: 'loader.command', category: 'command', args: { match: ['*.vsdx'], command: [process.execPath, fixture, '{input}'], input: 'file',
        output: 'markdown', env: { PARSER_MODE: 'fail' }, version: '1', timeout_ms: 20000, concurrency: 2 } },
      { name: 'loader.auto', category: 'auto', args: { ocr: 'none' } },
    ]);
    const result = await compare(t, files, routes, { rounds: 1 });
    assert.match(result.outputs[0], /"error":"Could not load docs:/u);
  }
});
