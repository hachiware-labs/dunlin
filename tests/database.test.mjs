import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setImmediate } from 'node:timers/promises';
import { promisify } from 'node:util';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';

import { Dunlin, DunlinError } from 'dunlin';

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const docs = join(root, 'docs');
  await mkdir(docs);
  return { root, docs, store: join(root, 'index'), source: { id: 'docs', kind: 'filesystem', path: docs } };
}

function errorCode(code) {
  return (error) => error instanceof DunlinError && error.code === code;
}

test('keeps full corpus revisions when location metadata or the original changes', async (t) => {
  const { store } = await fixture(t);
  const ref = { sourceId: 'parsed', objectId: 'manual.pdf' };
  const input = { objectId: ref.objectId, url: 'https://example.com/manual.pdf', format: 'text', content: 'Overview\r\nRetention\r\n' };
  const location = { page: 2, range: { startLine: 2, endLine: 2 }, bbox: [10, 20, 300, 100] };
  const parser = { tool: 'ocr', model: 'fixture' };
  const database = await Dunlin.open(store);
  let versions;
  try {
    const revisions = [];
    for (const [page, originalSha256] of [[2, 'a'.repeat(64)], [3, 'a'.repeat(64)], [3, 'b'.repeat(64)]]) {
      const corpus = { locations: [{ ...location, page }], parser, originalSha256 };
      await database.syncDocuments(ref.sourceId, [{ ...input, corpus }]);
      revisions.push((await database.getDocument(ref)).evidence.source.version);
    }
    versions = revisions;
    assert.equal(new Set(versions).size, 3);
    await database.removeSource(ref.sourceId);
    assert.equal(await database.getDocument(ref), null);
  } finally {
    await database.close();
  }
  const reopened = await Dunlin.open(store);
  try {
    const [first, second, third] = await Promise.all(versions.map((version) => reopened.getCorpusDocument(ref, version)));
    assert.equal(first.content, input.content);
    assert.deepEqual(first.locations, [location]);
    assert.deepEqual(first.parser, parser);
    assert.equal(second.locations[0].page, 3);
    assert.notEqual(first.parsedVersion, second.parsedVersion);
    assert.equal(second.parsedVersion, third.parsedVersion);
    assert.equal(third.originalSha256, 'b'.repeat(64));
  } finally {
    await reopened.close();
  }
});

test('indexes Markdown, Unicode and code identifiers with verifiable line evidence', async (t) => {
  const { docs, store, source } = await fixture(t);
  const file = join(docs, '仕様 #1.md');
  await writeFile(file, '# はじめに\r\n背景の説明。\r\n## 認証\r\n新しい認証方式はOAuthです。🔑\r\n## 監査\r\n変更履歴');
  await writeFile(join(docs, 'auth.ts'), 'export const OAUTH_TOKEN = "bearer";');
  await writeFile(join(docs, '.hidden.md'), 'private_marker');
  await mkdir(join(docs, 'node_modules'));
  await writeFile(join(docs, 'node_modules', 'skip.js'), 'private_marker');

  const database = await Dunlin.open(store);
  try {
    assert.deepEqual(await database.addSource(source), { files: 2, units: 4 });
    const result = await database.search('認証方式');
    assert.equal(result.totalHits, 1);
    assert.equal(result.status, 'fewer_matches');
    const { title, evidence } = result.hits[0];
    assert.match(title, /認証$/);
    assert.deepEqual(evidence.range, { startLine: 3, endLine: 4 });
    assert.equal(evidence.source.sourceId, 'docs');
    assert.equal(fileURLToPath(evidence.source.url), file);
    assert.equal(typeof evidence.acquiredAtMs, 'number');
    assert.equal((await database.read(evidence)).content, '## 認証\r\n新しい認証方式はOAuthです。🔑\r\n');
    const exactLine = { ...evidence, range: { startLine: 4, endLine: 4 } };
    assert.equal((await database.read(exactLine)).content, '新しい認証方式はOAuthです。🔑\r\n');
    assert.equal((await database.search('oauth_token', { limit: 1 })).hits[0].title, 'auth.ts');
    assert.equal((await database.search('private_marker')).totalHits, 0);
    assert.equal((await database.search('式は')).totalHits, 1);
    await assert.rejects(database.read({ ...evidence, range: { startLine: 4, endLine: 999 } }), errorCode('INVALID_ARGUMENT'));
    await assert.rejects(database.read({ ...evidence, source: { ...evidence.source, url: 'file:///elsewhere' } }), errorCode('STALE_EVIDENCE'));
  } finally {
    await database.close();
  }
});

test('persists snapshots, rolls back failed replacements and rejects stale evidence', async (t) => {
  const { docs, store, source } = await fixture(t);
  const path = join(docs, 'main.txt');
  await writeFile(path, 'original marker');
  let database = await Dunlin.open(store);
  try {
    await database.addSource(source);
    const old = (await database.search('original')).hits[0].evidence;
    await database.close();
    database = await Dunlin.open(store);
    await writeFile(path, 'replacement marker');
    const invalid = join(docs, 'broken.txt');
    await writeFile(invalid, Buffer.from([0xff]));
    await assert.rejects(database.addSource(source), errorCode('IO_ERROR'));
    assert.equal((await database.read(old)).content, 'original marker');
    assert.equal((await database.search('replacement')).totalHits, 0);

    await rm(invalid);
    await database.addSource(source);
    await assert.rejects(database.read(old), errorCode('STALE_EVIDENCE'));
    const current = (await database.search('replacement')).hits[0].evidence;
    const unrelated = join(docs, 'separate.md');
    await writeFile(unrelated, '# Separate source\nother marker');
    await database.addSource({ id: 'extra', kind: 'filesystem', path: unrelated });
    assert.equal((await database.search('marker')).totalHits, 2);
    await database.removeSource(source.id);
    await assert.rejects(database.read(current), errorCode('NOT_FOUND'));
    assert.equal((await database.search('marker')).hits[0].evidence.source.sourceId, 'extra');
  } finally {
    await database.close();
  }
});

test('replaces imported documents atomically and reads exact lines with their original URLs', async (t) => {
  const { store } = await fixture(t);
  const web = {
    objectId: 'auth.md', url: 'https://Example.invalid/auth?edition=1#oauth', format: 'markdown',
    content: '# Overview\r\nBackground\r\n## OAuth\r\n検証済みの根拠 🔑\r\n## End\r\nOther content',
  };
  const object = {
    objectId: 'policy.txt', url: 's3://example-bucket/docs/policy.txt', format: 'text',
    content: 'retention policy',
  };
  let database = await Dunlin.open(store);
  try {
    assert.deepEqual(await database.replaceDocuments('imported', [web, object]), { files: 2, units: 4 });
    await database.close();
    database = await Dunlin.open(store);

    const { hits } = await database.search('検証済み', { sourceIds: ['imported'] });
    assert.equal(hits.length, 1);
    const { evidence } = hits[0];
    assert.equal(evidence.source.sourceId, 'imported');
    assert.equal(evidence.source.objectId, web.objectId);
    assert.equal(evidence.source.url, web.url);
    assert.deepEqual(evidence.range, { startLine: 3, endLine: 4 });
    assert.equal((await database.read(evidence)).content, '## OAuth\r\n検証済みの根拠 🔑\r\n');
    const removed = (await database.search('retention')).hits[0].evidence;
    assert.equal(removed.source.url, object.url);

    const updated = { ...web, content: 'updated body' };
    const invalidDocuments = [
      updated,
      { ...object, url: 'docs/policy.txt' },
      { ...object, content: 'a'.repeat(1024 * 1024 + 1) },
    ];
    for (const invalid of invalidDocuments) {
      await assert.rejects(database.replaceDocuments('imported', [updated, invalid]), errorCode('INVALID_ARGUMENT'));
      assert.equal((await database.search('updated')).totalHits, 0);
      assert.equal((await database.read(removed)).content, object.content);
    }
    await assert.rejects(database.replaceDocuments('imported', [updated], { signal: AbortSignal.abort() }), errorCode('ABORTED'));
    assert.equal((await database.search('検証済み')).totalHits, 1);

    await database.replaceDocuments('imported', [updated]);
    await assert.rejects(database.read(evidence), errorCode('STALE_EVIDENCE'));
    await assert.rejects(database.read(removed), errorCode('NOT_FOUND'));
    const current = (await database.search('updated')).hits[0].evidence;
    assert.equal((await database.read(current)).content, updated.content);

    await database.replaceDocuments('imported', [{ ...updated, url: 'https://example.invalid/new-location' }]);
    await assert.rejects(database.read(current), errorCode('STALE_EVIDENCE'));
    assert.deepEqual(await database.replaceDocuments('imported', []), { files: 0, units: 0 });
    assert.equal((await database.search('updated')).totalHits, 0);
  } finally {
    await database.close();
  }
});

test('rejects invalid inputs and propagates abort and close to native work', async (t) => {
  const { docs, store, source } = await fixture(t);
  await assert.rejects(Dunlin.open(store, { signal: AbortSignal.abort() }), errorCode('ABORTED'));
  const database = await Dunlin.open(store);
  try {
    await assert.rejects(database.search('%%%'), errorCode('INVALID_ARGUMENT'));
    await assert.rejects(database.search('word', { limit: 1.5 }), errorCode('INVALID_ARGUMENT'));
    await assert.rejects(database.search('word', { mode: 'vector' }), errorCode('INVALID_ARGUMENT'));
    await assert.rejects(database.search('word', { signal: AbortSignal.abort() }), errorCode('ABORTED'));
    for (let i = 0; i < 48; i++) {
      await writeFile(join(docs, `doc-${i}.txt`), 'rocket abort_marker\n'.repeat(16_000));
    }
    const controller = new AbortController();
    const syncing = database.addSource(source, { signal: controller.signal });
    const rejected = assert.rejects(syncing, errorCode('ABORTED'));
    await setImmediate();
    controller.abort();
    await rejected;
    assert.equal((await database.search('abort_marker')).totalHits, 0);

    const closing = database.addSource(source);
    const interrupted = assert.rejects(closing, errorCode('ABORTED'));
    await database.close();
    await interrupted;
    await database.close();
    await assert.rejects(database.search('rocket'), errorCode('CLOSED'));
  } finally {
    await database.close();
  }
});

test('search deadline includes time spent in the native work queue', async (t) => {
  const { store } = await fixture(t);
  const child = new URL('./fixtures/deadline.mjs', import.meta.url);
  await promisify(execFile)(process.execPath, [fileURLToPath(child), store], {
    env: { ...process.env, UV_THREADPOOL_SIZE: '1' },
    timeout: 20_000,
  });
});
