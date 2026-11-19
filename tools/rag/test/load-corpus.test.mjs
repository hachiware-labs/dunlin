import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import { DatabaseSync } from 'node:sqlite';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { loadCorpus } from './fixtures/cli-reference/project-content.mjs';
import { defaultRoutes } from './fixtures/cli-reference/project-parsers.mjs';

async function fixture(t, files) {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-load-'));
  const docs = join(root, 'docs');
  await mkdir(docs);
  for (const [name, body] of files) await writeFile(join(docs, name), body);
  // The loader opens state.sqlite itself, so the test uses a file.
  const state = join(root, 'state.sqlite');
  const db = new DatabaseSync(state);
  // Close the database before removing its directory.
  t.after(() => {
    db.close();
    return rm(root, { recursive: true, force: true, maxRetries: 5 });
  });
  const sources = [{ id: 'docs', files: files.map(([name]) => ({ file: join(docs, name), objectId: name })) }];
  const artifacts = join(root, 'artifacts');
  const load = () => loadCorpus(sources, { artifacts, state, corpus: join(root, 'corpus') }, [], [], db, false, defaultRoutes(), undefined, root);
  const shared = join(artifacts, 'originals', 'shared');
  return { db, load, shared };
}

const sha256 = (text) => createHash('sha256').update(text).digest('hex');

test('reads more files than one native batch and stores each original once', async (t) => {
  const files = Array.from({ length: 600 }, (_, i) => [`d-${String(i).padStart(3, '0')}.md`, `# 文書 ${i % 300}\n本文\n`]);
  const { db, load, shared } = await fixture(t, files);
  const [[, documents]] = await load();
  assert.equal(documents.length, 600);
  assert.deepEqual(documents.map(({ objectId }) => objectId), files.map(([name]) => name));
  assert.equal(documents[1].content, '# 文書 1\n本文\n');
  assert.equal(documents[1].corpus.originalSha256, sha256('# 文書 1\n本文\n'));
  assert.equal(typeof documents[1].modifiedAtMs, 'number');
  const stored = await readdir(shared);
  assert.equal(stored.length, 300);
  assert.equal(await readFile(join(shared, sha256('# 文書 7\n本文\n') + '.bin'), 'utf8'), '# 文書 7\n本文\n');
  assert.equal(db.prepare('SELECT COUNT(*) AS count FROM extractions').get().count, 600);
  const [[, again]] = await load();
  assert.deepEqual(again, documents.map((document) => JSON.parse(JSON.stringify(document))));
});

test('keeps the originals and cache rows of files read before a failure', async (t) => {
  const { db, load, shared } = await fixture(t, [['a.md', '# A\n'], ['b.md', '# B\n'], ['c.txt', Buffer.from([0xff])]]);
  await assert.rejects(load(), /Could not load docs:c\.txt\. The encoded data was not valid/u);
  assert.deepEqual((await readdir(shared)).sort(), [sha256('# A\n') + '.bin', sha256('# B\n') + '.bin'].sort());
  assert.equal(db.prepare('SELECT COUNT(*) AS count FROM extractions').get().count, 2);
});

test('reports an original that conflicts with its digest', async (t) => {
  const { load, shared } = await fixture(t, [['a.md', '# A\n']]);
  await mkdir(shared, { recursive: true });
  await writeFile(join(shared, sha256('# A\n') + '.bin'), 'tampered');
  await assert.rejects(load(), /Could not load docs:a\.md\. Stored original file does not match its SHA-256 digest/u);
});
