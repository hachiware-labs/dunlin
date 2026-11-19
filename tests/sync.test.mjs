import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, stat, utimes, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { Dunlin } from 'dunlin';

const document = (objectId, content, format = 'text') => ({
  objectId, content, format, url: `https://example.invalid/${objectId}`,
});

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-sync-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const path = join(root, 'docs');
  const store = join(root, 'index');
  await mkdir(path);
  return { store, path, source: { id: 'docs', kind: 'filesystem', path } };
}

test('filesystem sync writes only changed objects', async (t) => {
  const { path, store, source } = await fixture(t);
  const changedPath = join(path, 'changed.md');
  const deletedPath = join(path, 'deleted.md');
  const stablePath = join(path, 'stable.md');
  await writeFile(changedPath, '# old_version\nFirst section\n## Archive\nold_version\n');
  await writeFile(deletedPath, '# removed_marker\nFirst section\n## Archive\nremoved_marker\n');
  await writeFile(stablePath, '# keep_marker\nOriginal stable evidence\n');
  const kb = await Dunlin.open(store);
  try {
    const initial = await kb.syncSource(source);
    assert.deepEqual(initial.added, ['changed.md', 'deleted.md', 'stable.md']);
    assert.equal(initial.indexedUnits, 5);

    const stableBefore = (await kb.search('keep_marker')).hits[0].evidence;
    const oldVersion = (await kb.search('old_version')).hits[0].evidence;
    const oldRemoved = (await kb.search('removed_marker')).hits[0].evidence;
    const oldTimes = await stat(changedPath);
    await writeFile(changedPath, '# new_version\nFirst section\n## Archive\nnew_version\n');
    await utimes(changedPath, oldTimes.atime, oldTimes.mtime);
    await utimes(stablePath, new Date(), new Date());
    await rm(deletedPath);
    await writeFile(join(path, 'added.md'), '# added_marker\n');

    const result = await kb.syncSource(source);
    assert.deepEqual(result.added, ['added.md']);
    assert.deepEqual(result.updated, ['changed.md']);
    assert.deepEqual(result.deleted, ['deleted.md']);
    assert.equal(result.unchanged, 1);
    assert.equal(result.indexedUnits, 3);
    assert.equal((await kb.search('old_version')).totalHits, 0);
    assert.equal((await kb.search('removed_marker')).totalHits, 0);
    assert.equal((await kb.search('new_version')).totalHits, 2);
    assert.deepEqual((await kb.search('keep_marker')).hits[0].evidence, stableBefore);
    await assert.rejects(kb.read(oldVersion), { code: 'STALE_EVIDENCE' });
    await assert.rejects(kb.read(oldRemoved), { code: 'NOT_FOUND' });

    const noop = await kb.syncSource(source);
    assert.deepEqual(noop, {
      sourceId: 'docs', added: [], updated: [], deleted: [], unchanged: 3, indexedUnits: 0,
    });
  } finally {
    await kb.close();
  }
});

test('document sync tracks format and URL changes and rolls back a rejected input', async (t) => {
  const { store } = await fixture(t);
  const kb = await Dunlin.open(store);
  try {
    const text = document('guide', '# Intro\nExplanation\n## Rules\nformat_marker');
    const reference = document('reference', 'reference_marker');
    const blank = document('blank', '');
    await kb.replaceDocuments('remote', [text, reference, blank]);
    const newReference = { ...reference, url: 'https://example.invalid/revised' };
    const next = [{ ...text, format: 'markdown' }, newReference, blank];
    await assert.rejects(kb.syncDocuments('remote', [...next, blank]), { code: 'INVALID_ARGUMENT' });
    assert.equal((await kb.search('reference_marker')).hits[0].evidence.source.url, reference.url);
    await assert.rejects(kb.syncDocuments('remote', [], { signal: AbortSignal.abort() }), { code: 'ABORTED' });

    const result = await kb.syncDocuments('remote', next);
    assert.deepEqual(result.updated, ['guide', 'reference']);
    assert.equal(result.unchanged, 1);
    assert.equal(result.indexedUnits, 3);
    assert.deepEqual((await kb.search('format_marker')).hits[0].evidence.range, { startLine: 3, endLine: 4 });
    assert.equal((await kb.search('reference_marker')).hits[0].evidence.source.url, newReference.url);

    const empty = await kb.syncDocuments('remote', []);
    assert.deepEqual(empty.deleted, ['blank', 'guide', 'reference']);
    assert.equal((await kb.search('reference_marker')).totalHits, 0);
  } finally {
    await kb.close();
  }
});
