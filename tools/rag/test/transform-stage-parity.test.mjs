import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { createServer } from 'node:http';
import { mkdir, mkdtemp, rm } from 'node:fs/promises';
import { DatabaseSync } from 'node:sqlite';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { runTransformStage } from '../project-content.mjs';
import { referenceTransformStage } from './fixtures/transform-stage-reference.mjs';

const schema = `CREATE TABLE stage_outputs (stage TEXT NOT NULL, id TEXT NOT NULL, manifest TEXT NOT NULL, PRIMARY KEY (stage, id));
  CREATE TABLE latest_stage (stage TEXT PRIMARY KEY, id TEXT NOT NULL);
  CREATE TABLE stage_payloads (id TEXT PRIMARY KEY, data TEXT NOT NULL);
  CREATE TABLE document_chunks (id TEXT PRIMARY KEY, chunks TEXT NOT NULL);
  CREATE TABLE transform_outputs (key TEXT PRIMARY KEY, output_id TEXT NOT NULL);`;

const revision = (content) => createHash('sha256').update(content).digest('hex');

const manual = '# 点検手順\n電源を止めます。フィルタを外します。\n## 校正\nCalibration ＡＢＣ①. Replace the filter.\n\n点検は毎月です。　\n';
const groups = [
  ['docs', [
    { objectId: 'manual.md', format: 'markdown', corpus: { locations: [
      { page: 1, range: { startLine: 1, endLine: 2 } }, { page: 2, range: { startLine: 3, endLine: 4 } },
      { page: 2, range: { startLine: 3, endLine: 4 } }, { page: 0, range: { startLine: 5, endLine: 5 } }] }, content: manual },
    { objectId: 'notes.txt', format: 'text', content: 'Calibration notes. Check the sensor.\r\nThen calibrate again. Done!\n' },
    { objectId: 'empty.txt', format: 'text', content: '' },
    { objectId: 'blank.txt', format: 'text', content: '   \n　\n﻿\n' },
    { objectId: 'plain', content: 'No format here. 点検 only.' },
  ]],
  ['more', [{ objectId: 'copy.md', format: 'markdown', content: manual }]],
  ['none', []],
];

/** Records the source stage as Rust does: contents as payloads. */
function saveSource(db) {
  const insert = db.prepare('INSERT OR IGNORE INTO stage_payloads VALUES (?, ?)');
  const stored = groups.map(([id, documents]) => [id, documents.map(({ content, ...document }) => {
    const json = JSON.stringify(content);
    insert.run(revision(json), json);
    return { ...document, text: revision(json) };
  })]);
  db.prepare("INSERT INTO stage_outputs VALUES ('source', 'src-test', ?)").run(JSON.stringify({ id: 'src-test', groups: stored, payloads: 1 }));
}

const entry = (use, args) => ({ use, version: 1, name: use, with: args });
const normalize = (unicode) => entry('text.normalize', { unicode });
const semantic = (args) => entry('chunk.semantic', { model: 'embedder', max_chars: 40, sentence_chars: 10, similarity: 0.55, ...args });

/** A vector by topic, with digits that need exact float parsing. */
function vector(text) {
  const noise = (text.length % 7) * 0.0123456789012345;
  if (text.includes('点検')) return [1, 0.2 + noise, 1e-7];
  if (/calibrat/iu.test(text)) return [0.1 + noise, 1, 0];
  return [noise, 0.3, 1.0000000000000002];
}

const rows = (db) => Object.fromEntries(['stage_outputs', 'stage_payloads', 'latest_stage', 'document_chunks', 'transform_outputs']
  .map((table) => [table, db.prepare(`SELECT * FROM ${table} ORDER BY 1, 2`).all()]));

test('runs the transform stage with the identifiers, cache and rows of the JavaScript implementation', async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-transform-stage-'));
  const bodies = [];
  const server = createServer((request, response) => {
    let body = '';
    request.setEncoding('utf8');
    request.on('data', (part) => { body += part; });
    request.on('end', () => {
      bodies.push(body);
      const { input } = JSON.parse(body);
      const reply = request.url === '/api/embed' ? { embeddings: input.map(vector) }
        : { data: input.map((text, index) => ({ index, embedding: vector(text) })).reverse() };
      response.setHeader('content-type', 'application/json');
      response.end(JSON.stringify(reply));
    });
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const base = `http://127.0.0.1:${server.address().port}`;
  const open = [];
  // Close the databases before removing their directory.
  t.after(() => {
    server.close();
    for (const db of open) db.close();
    return rm(root, { recursive: true, force: true, maxRetries: 5 });
  });
  const project = async (name) => {
    await mkdir(join(root, name));
    const state = join(root, name, 'state.sqlite');
    const db = new DatabaseSync(state);
    db.exec(schema);
    saveSource(db);
    open.push(db);
    return { db, paths: { state } };
  };
  const actual = await project('actual');
  const expected = await project('expected');

  const ollama = { provider: 'ollama', model: 'embed-model', url: `${base}/api/embed` };
  const openai = { provider: 'openai', model: 'text-embedding', url: `${base}/v1/embeddings`, apiKey: 'test-key', maxInputBytes: 100_000 };
  const plans = [
    [[normalize('nfkc'), entry('chunk.length', { unit: 'characters', size: 12, overlap: 3 })]],
    [[entry('chunk.structure', { max_chars: 15 })]],
    [[normalize('none'), semantic({})], ollama],
    [[normalize('nfkc'), semantic({ similarity: 0.3 })], openai],
    [[normalize('nfkc'), entry('chunk.length', { unit: 'characters', size: 12, overlap: 3 })]],
  ];
  for (const [modules, embedder = null] of plans) {
    for (const force of [false, false, true]) {
      const request = { sourceId: 'src-test', modules, force, embedder };
      bodies.length = 0;
      const result = await runTransformStage(request, actual.paths);
      const sent = bodies.splice(0);
      const reference = JSON.parse(JSON.stringify(await referenceTransformStage(expected.db, request)));
      assert.deepEqual(result, reference);
      // The same request bodies, so trial limits count the same bytes.
      assert.deepEqual(sent, bodies);
      assert.deepEqual(rows(actual.db), rows(expected.db));
      if (embedder) {
        // Requests go out unless the stage is reused, and similar passages are joined.
        assert.equal(sent.length > 0, !result.reused);
        const passages = result.output.chunks[0][0];
        assert.ok(passages.some(({ byteRange }) => Buffer.from(manual).subarray(byteRange.start, byteRange.end).toString().length > 10));
      }
    }
  }
  assert.ok(rows(actual.db).transform_outputs.length >= 2);

  const limited = { sourceId: 'src-test', modules: [semantic({ max_chars: 41 })], embedder: { ...ollama, maxInputBytes: 20 } };
  await assert.rejects(runTransformStage(limited, actual.paths), { code: 'MODEL_INPUT_LIMIT', exitCode: 2, message: 'The model request exceeds max_input_bytes.' });
  await assert.rejects(referenceTransformStage(expected.db, limited), { code: 'MODEL_INPUT_LIMIT', message: 'The model request exceeds max_input_bytes.' });
  await assert.rejects(runTransformStage({ sourceId: 'src-missing', modules: plans[0][0] }, actual.paths), { code: 'STAGE_REQUIRED', exitCode: 3 });
});
