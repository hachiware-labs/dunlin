import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { Dunlin } from 'dunlin';

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-explore-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const docs = {
    notes: {
      'start.md': [
        '# root_marker',
        '[Auth][spec]',
        '[Local note](./local.md)',
        '',
        '[spec]: ../architecture/' + encodeURIComponent('認証 仕様.md') + '#auth',
        '',
        '`[Not a link](./fake.md)`',
        '[External](https://example.invalid/doc.md)',
      ].join('\n'),
      'local.md': '# Local decision\n[Start](./start.md)',
    },
    architecture: {
      '認証 仕様.md': '# Authentication\n[Policy](dunlin://Operations/rules.txt)\n[Start](dunlin://notes/start.md)',
    },
    Operations: {
      'rules.txt': 'Policy evidence\n' + 'Detail\n'.repeat(40),
    },
    tickets: {
      'ticket.md': '# root_marker\n[Missing](../outside.md)\n[Empty](dunlin://tickets/empty.txt)',
      'empty.txt': '',
    },
  };
  const database = await Dunlin.open(join(root, 'index'));
  try {
    await writeFile(join(root, 'outside.md'), 'Unregistered private file');
    for (const [id, files] of Object.entries(docs)) {
      const path = join(root, id);
      await mkdir(path);
      for (const [name, text] of Object.entries(files)) {
        await writeFile(join(path, name), text);
      }
      await database.addSource({ id, kind: 'filesystem', path });
    }
    await rm(join(root, 'architecture', '認証 仕様.md'));
    return database;
  } catch (error) {
    await database.close();
    throw error;
  }
}

function sources(items) {
  return items.map(({ evidence }) => evidence.source.sourceId);
}

test('searches registered sources and follows cross-source Markdown links through multiple hops', async (t) => {
  const database = await fixture(t);
  try {
    const found = await database.search('root_marker');
    assert.deepEqual(sources(found.hits).sort(), ['notes', 'tickets']);
    const result = await database.explore('root_marker', { seedLimit: 2 });
    assert.equal(result.status, 'partial');
    assert.equal(result.stopReason, 'exhausted');
    assert.deepEqual(sources(result.evidence).sort(), ['Operations', 'architecture', 'notes', 'notes', 'tickets']);
    assert.deepEqual(sources(result.evidence.filter(({ via }) => via.kind === 'search')).sort(), ['notes', 'tickets']);

    const architecture = result.evidence.find(({ evidence }) => evidence.source.sourceId === 'architecture');
    assert.equal(architecture.evidence.source.objectId, '認証 仕様.md');
    assert.equal(architecture.via.kind, 'link');
    assert.equal(architecture.via.from.objectId, 'start.md');
    assert.equal(architecture.via.line, 2);
    const policy = result.evidence.find(({ evidence }) => evidence.source.sourceId === 'Operations');
    assert.equal(policy.via.from.sourceId, 'architecture');
    assert.equal(policy.via.destination, 'dunlin://Operations/rules.txt');
    assert.deepEqual(policy.evidence.range, { startLine: 1, endLine: 32 });
    assert.equal((await database.read(policy.evidence)).content, 'Policy evidence\n' + 'Detail\n'.repeat(31));
    assert.equal(result.evidence.filter(({ evidence }) => evidence.source.objectId === 'start.md').length, 1);
    assert.deepEqual(result.issues.map(({ code, line }) => [code, line]), [
      ['unresolved_link', 2],
      ['empty_target', 3],
    ]);
  } finally {
    await database.close();
  }
});

test('source scope applies to ranking and link traversal, and results identify stopping conditions', async (t) => {
  const database = await fixture(t);
  try {
    await database.activatePlan('keyword-coverage-v1');
    const global = await database.search('root_marker missing');
    const scoped = await database.search('root_marker missing', { sourceIds: ['notes', 'architecture'] });
    assert.deepEqual(sources(scoped.hits), ['notes']);
    assert.equal(scoped.totalHits, 1);
    assert.equal(global.totalHits, 2);

    const { evidence, planId, status } = await database.explore('root_marker', {
      sourceIds: ['notes', 'architecture'],
    });
    assert.equal(planId, 'keyword-coverage-v1');
    assert.equal(status, 'complete');
    assert.deepEqual(sources(evidence).sort(), ['architecture', 'notes', 'notes']);
    const confined = await database.explore('root_marker', { sourceIds: ['notes'] });
    assert.deepEqual(sources(confined.evidence), ['notes', 'notes']);
    assert.deepEqual(confined.issues.map(({ code }) => code), ['unresolved_link']);

    const limited = await database.explore('root_marker', { sourceIds: ['notes', 'architecture'], maxSteps: 3 });
    assert.equal(limited.steps, 3);
    assert.equal(limited.status, 'partial');
    assert.equal(limited.stopReason, 'step_limit');
    assert.deepEqual(sources(limited.evidence), ['notes', 'architecture']);
    const cancelled = await database.explore('root_marker', { signal: AbortSignal.abort() });
    assert.equal(cancelled.status, 'partial');
    assert.equal(cancelled.stopReason, 'cancelled');
    assert.equal(cancelled.steps, 0);
    assert.deepEqual(cancelled.evidence, []);
    const empty = await database.explore('nonexistent_marker');
    assert.equal(empty.status, 'no_matches');
    assert.equal(empty.stopReason, 'exhausted');
    assert.equal(empty.steps, 1);
    await assert.rejects(database.explore('root_marker', { sourceIds: [] }), { code: 'INVALID_ARGUMENT' });
    await assert.rejects(database.explore('root_marker', { maxSteps: 0 }), { code: 'INVALID_ARGUMENT' });
    await assert.rejects(database.explore('%%%'), { code: 'INVALID_ARGUMENT' });
  } finally {
    await database.close();
  }
});
