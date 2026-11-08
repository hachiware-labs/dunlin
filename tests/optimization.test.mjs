import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { Dunlin } from 'dunlin';

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-plans-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const docs = join(root, 'docs');
  await mkdir(docs);
  const terms = [['alpha', 'beta'], ['nonce', 'rotation'], ['認証方式', '監査記録']];
  const evaluationSet = [];
  for (const [index, [first, second]] of terms.entries()) {
    const objectId = `target-${index}.txt`;
    await writeFile(join(docs, objectId), `${first} ${second} ${'padding '.repeat(250)}`);
    await writeFile(join(docs, `${first}.txt`), `${first} `.repeat(60));
    for (let decoy = 0; decoy < 8; decoy++) {
      await writeFile(join(docs, `decoy-${index}-${decoy}.txt`), second);
    }
    evaluationSet.push({ query: `${first} ${second}`, relevant: [{ sourceId: 'docs', objectId }] });
  }
  const jsonl = join(root, 'questions.jsonl');
  await writeFile(jsonl, '\uFEFF' + evaluationSet.map((item) => JSON.stringify(item)).join('\r\n') + '\r\n');
  return { store: join(root, 'index'), docs, evaluationSet, jsonl };
}

test('compares retrieval strategies on the same labeled cases and preserves evidence', async (t) => {
  const { store, docs, evaluationSet, jsonl } = await fixture(t);
  const database = await Dunlin.open(store);
  try {
    await database.addSource({ id: 'docs', kind: 'filesystem', path: docs });
    const [example] = evaluationSet;
    const keyword = await database.search(example.query, { mode: 'keyword' });
    const coverage = await database.search(example.query, { mode: 'coverage' });
    assert.equal(keyword.planId, 'keyword-v1');
    assert.equal(coverage.planId, 'keyword-coverage-v1');
    assert.equal(coverage.hits[0].evidence.source.objectId, example.relevant[0].objectId);
    assert.equal(coverage.totalHits, keyword.totalHits);
    assert.equal(coverage.hits.length, 10);
    assert.equal(new Set(coverage.hits.map(({ evidence }) => evidence.source.objectId)).size, 10);
    assert.match((await database.read(coverage.hits[0].evidence)).content, /^alpha beta /u);

    const report = await database.evaluate({ evaluationSet: jsonl, limit: 1, repetitions: 4 });
    assert.equal(report.queryCount, 3);
    const metrics = Object.fromEntries(report.candidates.map((candidate) => [candidate.method, candidate]));
    assert.equal(metrics.keyword.recallAtK, 0);
    assert.equal(metrics.keyword.ndcgAtK, 0);
    assert.equal(metrics.coverage.recallAtK, 1);
    assert.equal(metrics.coverage.ndcgAtK, 1);
    for (const candidate of report.candidates) {
      assert.equal(candidate.latencySamples, 12);
      assert.ok(Number.isFinite(candidate.p95LatencyMs) && candidate.p95LatencyMs >= 0);
    }
    const inMemory = await database.evaluate({ evaluationSet, limit: 1 });
    assert.equal(inMemory.evaluationSetHash, report.evaluationSetHash);
    assert.equal(inMemory.indexRevision, report.indexRevision);
  } finally {
    await database.close();
  }
});

test('saves the chosen plan, switches it after reopening, and supports rollback', async (t) => {
  const { store, docs, evaluationSet } = await fixture(t);
  let database = await Dunlin.open(store);
  try {
    await database.addSource({ id: 'docs', kind: 'filesystem', path: docs });
    const fallback = await database.getActivePlan();
    assert.equal(fallback.id, 'keyword-v1');
    assert.equal(fallback.evaluation, null);
    const options = { evaluationSet, limit: 1, repetitions: 3 };
    const report = await database.optimize({ ...options, p95LatencyBudgetMs: 500 });
    assert.equal(report.status, 'selected');
    assert.match(report.selectedPlanId, /^evaluated-[a-f0-9]{64}$/u);
    assert.equal((await database.getActivePlan()).id, fallback.id);
    await database.close();

    database = await Dunlin.open(store);
    const plan = await database.activatePlan(report.selectedPlanId);
    assert.equal(plan.method, 'coverage');
    assert.equal(plan.version, 1);
    assert.equal(plan.defaultLimit, 1);
    assert.equal(plan.evaluation.report.indexRevision, report.indexRevision);
    assert.equal(plan.evaluation.report.evaluationSetHash, report.evaluationSetHash);
    const [{ query }] = evaluationSet;
    const defaultSearch = await database.search(query);
    assert.equal(defaultSearch.planId, plan.id);
    assert.equal(defaultSearch.hits.length, 1);
    assert.equal((await database.search(query, { limit: 2 })).hits.length, 2);
    assert.equal((await database.search(query, { mode: 'keyword' })).planId, fallback.id);
    await database.close();

    database = await Dunlin.open(store);
    assert.equal((await database.getActivePlan()).id, plan.id);
    assert.equal((await database.search(query, { mode: 'auto' })).engine, 'tantivy-bm25-coverage');
    const unmet = await database.optimize({ ...options, p95LatencyBudgetMs: 1e-12 });
    assert.equal(unmet.status, 'budget_unmet');
    assert.equal(unmet.selectedPlanId, null);
    await assert.rejects(database.activatePlan('../active'), { code: 'PLAN_NOT_FOUND' });
    await assert.rejects(database.activatePlan(fallback.id, { signal: AbortSignal.abort() }), { code: 'ABORTED' });
    await assert.rejects(database.evaluate({ evaluationSet: [] }), { code: 'INVALID_ARGUMENT' });
    await assert.rejects(database.evaluate({ ...options, signal: AbortSignal.abort() }), { code: 'ABORTED' });
    assert.equal((await database.getActivePlan()).id, plan.id);

    await database.activatePlan(fallback.id);
    await database.close();
    database = await Dunlin.open(store);
    assert.equal((await database.search(query)).planId, fallback.id);
  } finally {
    await database.close();
  }
});
