import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, readdir, rm, stat, utimes, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { Dunlin } from 'dunlin';

const ref = (sourceId, objectId) => ({ sourceId, objectId });
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

test('filesystem sync writes only changed objects and regenerates only affected Wiki pages', async (t) => {
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
    assert.deepEqual(initial.wiki, { status: 'complete', regenerated: [], failures: [] });
    await kb.replaceDocuments('policies', [document('policy', 'Stable policy')]);
    const policy = ref('policies', 'policy');
    const build = (id, documents) => kb.wiki.build({ id, title: id, documents });
    await build('changed', [ref('docs', 'changed.md'), ref('docs', 'deleted.md')]);
    const stableWiki = await build('unaffected', [ref('docs', 'stable.md'), policy]);
    await build('restored', [ref('docs', 'added.md'), policy]);

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
    assert.deepEqual(result.wiki, {
      status: 'complete',
      regenerated: [
        { id: 'changed', revision: 2, status: 'insufficient_evidence' },
        { id: 'restored', revision: 2, status: 'current' },
      ],
      failures: [],
    });
    assert.equal((await kb.search('old_version')).totalHits, 0);
    assert.equal((await kb.search('removed_marker')).totalHits, 0);
    assert.equal((await kb.search('new_version')).totalHits, 2);
    assert.deepEqual((await kb.search('keep_marker')).hits[0].evidence, stableBefore);
    await assert.rejects(kb.read(oldVersion), { code: 'STALE_EVIDENCE' });
    await assert.rejects(kb.read(oldRemoved), { code: 'NOT_FOUND' });
    assert.deepEqual(await kb.wiki.get('unaffected'), stableWiki);
    assert.match((await kb.wiki.get('changed')).markdown, /new_version/u);
    assert.deepEqual(await kb.wiki.affected(), []);

    const noop = await kb.syncSource(source);
    assert.deepEqual(noop, {
      sourceId: 'docs', added: [], updated: [], deleted: [], unchanged: 3, indexedUnits: 0,
      wiki: { status: 'complete', regenerated: [], failures: [] },
    });
    assert.equal((await kb.wiki.get('changed')).revision, 2);
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
    await kb.wiki.build({
      id: 'remote/guide', title: 'Remote guide',
      documents: [ref('remote', 'guide'), ref('remote', 'reference')],
    });
    const newReference = { ...reference, url: 'https://example.invalid/revised' };
    const next = [{ ...text, format: 'markdown' }, newReference, blank];
    await assert.rejects(kb.syncDocuments('remote', [...next, blank]), { code: 'INVALID_ARGUMENT' });
    assert.equal((await kb.search('reference_marker')).hits[0].evidence.source.url, reference.url);
    assert.equal((await kb.wiki.get('remote/guide')).revision, 1);
    await assert.rejects(kb.syncDocuments('remote', [], { signal: AbortSignal.abort() }), { code: 'ABORTED' });

    const result = await kb.syncDocuments('remote', next);
    assert.deepEqual(result.updated, ['guide', 'reference']);
    assert.equal(result.unchanged, 1);
    assert.equal(result.indexedUnits, 3);
    assert.deepEqual((await kb.search('format_marker')).hits[0].evidence.range, { startLine: 3, endLine: 4 });
    assert.equal((await kb.search('reference_marker')).hits[0].evidence.source.url, newReference.url);
    assert.equal(result.wiki.regenerated[0].revision, 2);

    const empty = await kb.syncDocuments('remote', []);
    assert.deepEqual(empty.deleted, ['blank', 'guide', 'reference']);
    assert.equal(empty.wiki.regenerated[0].status, 'insufficient_evidence');
    assert.equal((await kb.search('reference_marker')).totalHits, 0);
  } finally {
    await kb.close();
  }
});

test('retries Wiki regeneration after an index commit even when input is unchanged', async (t) => {
  const { store } = await fixture(t);
  const kb = await Dunlin.open(store);
  try {
    const documents = [document('old', 'old_marker'), document('stable', 'stable evidence')];
    await kb.syncDocuments('remote', documents);
    await kb.wiki.build({
      id: 'retry', title: 'Retry',
      documents: [ref('remote', 'old'), ref('remote', 'stable')],
    });
    const wikiDir = join(store, '.dunlin-wiki-v1');
    const pageFile = join(wikiDir, (await readdir(wikiDir)).find((name) => name.endsWith('.json')));
    const pageData = await readFile(pageFile);
    await writeFile(pageFile, '{invalid');
    const newDocs = [document('old', 'new_marker'), documents[1]];
    const result = await kb.syncDocuments('remote', newDocs);
    assert.deepEqual(result.updated, ['old']);
    assert.equal(result.wiki.status, 'partial');
    assert.equal(result.wiki.failures[0].code, 'WIKI_ERROR');
    assert.equal((await kb.search('new_marker')).totalHits, 1);
    await writeFile(pageFile, pageData);

    const retried = await kb.syncDocuments('remote', newDocs);
    assert.deepEqual(retried.updated, []);
    assert.equal(retried.unchanged, 2);
    assert.equal(retried.indexedUnits, 0);
    assert.deepEqual(retried.wiki.regenerated, [{ id: 'retry', revision: 2, status: 'current' }]);
    assert.deepEqual(await kb.wiki.affected(), []);
  } finally {
    await kb.close();
  }
});
