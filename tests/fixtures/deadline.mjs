import assert from 'node:assert/strict';
import { pbkdf2 } from 'node:crypto';
import { promisify } from 'node:util';
import { KnowledgeNest } from 'knowledge-nest';

const database = await KnowledgeNest.open(process.argv[2]);
try {
  const occupyingWorker = promisify(pbkdf2)('knowledge-nest', 'fixture', 300_000, 32, 'sha256');
  const search = assert.rejects(database.search('keyword', { latencyBudgetMs: 1 }), { code: 'TIMEOUT' });
  const timedExplore = database.explore('keyword', { timeoutMs: 1 });
  const controller = new AbortController();
  const abortedExplore = database.explore('keyword', { signal: controller.signal });
  controller.abort();
  await occupyingWorker;
  await search;
  const timeout = await timedExplore;
  assert.equal(timeout.stopReason, 'timeout');
  assert.equal(timeout.steps, 0);
  assert.equal(timeout.status, 'partial');
  assert.deepEqual(timeout.evidence, []);
  const cancelled = await abortedExplore;
  assert.equal(cancelled.stopReason, 'cancelled');
  assert.equal(cancelled.steps, 0);
} finally {
  await database.close();
}
