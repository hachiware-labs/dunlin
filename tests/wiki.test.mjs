import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { Dunlin } from 'dunlin';

const overview = { sourceId: 'notes', objectId: 'overview.md' };
const details = { sourceId: 'notes', objectId: 'details.md' };
const authIssue = { sourceId: 'tickets', objectId: 'AUTH-42' };
const auditIssue = { sourceId: 'tickets', objectId: 'AUDIT-7' };
const originalOverview = '\r\n# OAuth\r\nInitial policy evidence.\r\n';
const originalIssue = 'The approval was documented in the issue.';

function ticket(reference, content) {
  return {
    objectId: reference.objectId,
    url: `https://example.invalid/issues/${reference.objectId}`,
    format: 'text',
    content,
  };
}

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-wiki-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const notes = join(root, 'notes');
  const store = join(root, 'index');
  const source = { id: 'notes', kind: 'filesystem', path: notes };
  const overviewPath = join(notes, 'overview.md');
  const audit = ticket(auditIssue, 'Audit scope is unchanged.');
  await mkdir(notes);
  await writeFile(overviewPath, originalOverview);
  await writeFile(join(notes, 'details.md'), 'Supplementary notes.');
  const kb = await Dunlin.open(store);
  try {
    await kb.addSource(source);
    await kb.replaceDocuments('tickets', [ticket(authIssue, originalIssue), audit]);
    return { kb, store, source, audit, overviewPath };
  } catch (error) {
    await kb.close();
    throw error;
  }
}

test('builds and reopens a Wiki page with verifiable citations from two sources', async (t) => {
  const { kb, store } = await fixture(t);
  let database = kb;
  try {
    assert.equal(await database.wiki.get('unknown/page'), null);
    assert.deepEqual(await database.wiki.affected(), []);
    const page = await database.wiki.build({
      id: 'architecture/authentication',
      title: '認証方式',
      documents: [overview, authIssue],
    });
    assert.equal(page.status, 'current');
    assert.equal(page.revision, 1);
    assert.ok(page.generatedAtMs > 0);
    assert.match(page.markdown, /^# 認証方式/u);
    assert.match(page.markdown, /Initial policy evidence/u);
    assert.match(page.markdown, /approval was documented/u);
    const [first, second] = page.citations.map(({ evidence }) => evidence);
    assert.deepEqual(first.range, { startLine: 2, endLine: 3 });
    assert.equal((await database.read(first)).content, originalOverview.slice(2));
    assert.equal((await database.read(second)).content, originalIssue);
    assert.equal(second.source.url, 'https://example.invalid/issues/AUTH-42');
    await database.close();
    database = await Dunlin.open(store);
    assert.deepEqual(await database.wiki.get(page.id), page);
    await assert.rejects(database.wiki.regenerate('unknown/page'), { code: 'WIKI_NOT_FOUND' });
    await assert.rejects(database.wiki.get('../private'), { code: 'INVALID_ARGUMENT' });
    await assert.rejects(database.wiki.build({
      id: 'duplicate', title: 'Duplicate', documents: [overview, overview],
    }), { code: 'INVALID_ARGUMENT' });
  } finally {
    await database.close();
  }
});

test('finds only affected pages and regenerates after source updates, deletions, and restoration', async (t) => {
  const { kb, source, audit, overviewPath } = await fixture(t);
  try {
    const target = await kb.wiki.build({ id: 'auth/main', title: 'Authentication', documents: [overview, authIssue] });
    await kb.wiki.build({ id: 'auth/secondary', title: 'Supporting notes', documents: [overview, details] });
    await kb.wiki.build({ id: 'audit', title: 'Audit', documents: [details, auditIssue] });

    await writeFile(overviewPath, '\n\n# OAuth\nUpdated policy evidence.');
    assert.deepEqual(await kb.wiki.affected(), []);
    await kb.addSource(source);
    await kb.replaceDocuments('tickets', [audit]);
    const impacts = await kb.wiki.affected();
    assert.deepEqual(impacts.map(({ id }) => id), ['auth/main', 'auth/secondary']);
    assert.deepEqual(impacts[0].changes.map(({ reason }) => reason), ['updated', 'deleted']);
    assert.equal(impacts[0].changes[0].previous.version, target.citations[0].evidence.source.version);
    assert.notEqual(impacts[0].changes[0].previous.version, impacts[0].changes[0].current.version);
    assert.equal(impacts[0].changes[1].current, null);
    assert.deepEqual((await kb.wiki.affected({ sourceId: 'tickets' })).map(({ id }) => id), ['auth/main']);
    assert.equal((await kb.wiki.get('audit')).status, 'current');

    const stale = await kb.wiki.get(target.id);
    assert.equal(stale.status, 'stale');
    assert.equal(stale.revision, 1);
    assert.deepEqual(stale.citations, []);
    assert.doesNotMatch(stale.markdown, /Initial policy|approval was documented/u);
    await assert.rejects(kb.wiki.regenerate(target.id, { signal: AbortSignal.abort() }), { code: 'ABORTED' });
    assert.equal((await kb.wiki.get(target.id)).revision, 1);

    const rebuilt = await kb.wiki.regenerate(target.id);
    assert.equal(rebuilt.revision, 2);
    assert.ok(rebuilt.generatedAtMs >= target.generatedAtMs);
    assert.equal(rebuilt.status, 'insufficient_evidence');
    assert.match(rebuilt.markdown, /Updated policy evidence/u);
    assert.deepEqual(rebuilt.citations[0].evidence.range, { startLine: 3, endLine: 4 });
    assert.deepEqual(rebuilt.unavailable, [{ document: authIssue, reason: 'missing' }]);
    assert.deepEqual((await kb.wiki.affected()).map(({ id }) => id), ['auth/secondary']);

    await kb.replaceDocuments('tickets', [ticket(authIssue, 'Restored approval evidence.'), audit]);
    const restored = await kb.wiki.get(target.id);
    assert.equal(restored.status, 'stale');
    assert.equal(restored.changes[0].reason, 'restored');
    assert.equal(restored.changes[0].previous, null);
    assert.equal(restored.citations.length, 1);
    const refreshed = await kb.wiki.regenerate(target.id);
    assert.equal(refreshed.status, 'current');
    assert.equal(refreshed.revision, 3);
    assert.match(refreshed.markdown, /Restored approval evidence/u);

    await kb.removeSource('notes');
    const deleted = await kb.wiki.get(target.id);
    assert.equal(deleted.changes[0].reason, 'deleted');
    assert.deepEqual(deleted.citations.map(({ number }) => number), [2]);
    assert.doesNotMatch(deleted.markdown, /Updated policy evidence/u);
  } finally {
    await kb.close();
  }
});

test('marks empty sources and clips a long UTF-8 excerpt without changing its evidence range', async (t) => {
  const { kb } = await fixture(t);
  try {
    const long = '界'.repeat(2000);
    await kb.replaceDocuments('tickets', [ticket(authIssue, long), ticket(auditIssue, '\n \t\n')]);
    const page = await kb.wiki.build({ id: 'large', title: '<draft>', documents: [authIssue, auditIssue] });
    assert.equal(page.status, 'insufficient_evidence');
    assert.deepEqual(page.unavailable, [{ document: auditIssue, reason: 'empty' }]);
    assert.ok(page.markdown.startsWith('# \\<draft\\>'));
    assert.ok(page.markdown.includes('界'.repeat(1365)));
    assert.ok(!page.markdown.includes('界'.repeat(1366)));
    assert.ok(page.citations[0].truncated);
    assert.equal((await kb.read(page.citations[0].evidence)).content, long);
  } finally {
    await kb.close();
  }
});
