import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { performance } from 'node:perf_hooks';
import { KnowledgeNest } from 'knowledge-nest';

const files = Number(process.argv[2] ?? 1_000);
const iterations = Number(process.argv[3] ?? 300);
if (![files, iterations].every((value) => Number.isSafeInteger(value) && value > 0)) {
  throw new Error('File and iteration counts must be positive integers');
}

function percentiles(samples) {
  const sorted = [...samples].sort((a, b) => a - b);
  const quantile = (q) => Number(sorted[Math.ceil(q * sorted.length) - 1].toFixed(3));
  return { p50: quantile(0.50), p95: quantile(0.95), p99: quantile(0.99) };
}

const root = await mkdtemp(join(tmpdir(), 'knowledge-nest-benchmark-'));
let database;
try {
  const docs = join(root, 'docs');
  const store = join(root, 'index');
  await mkdir(docs);
  const fileName = (id) => `document-${id}.md`;
  for (let id = 0; id < files; id++) {
    await writeFile(join(docs, fileName(id)), `# 認証方式\nOAuth sample feature_${id}.\n## 変更履歴\n設計を見直した。`);
  }

  let start = performance.now();
  database = await KnowledgeNest.open(store);
  const firstOpenMs = performance.now() - start;
  start = performance.now();
  const indexed = await database.addSource({ id: 'bench', kind: 'filesystem', path: docs });
  const ingestMs = performance.now() - start;
  await database.close();
  start = performance.now();
  database = await KnowledgeNest.open(store);
  const reopenMs = performance.now() - start;
  const firstSearchStart = performance.now();
  await database.search('feature_0 認証方式');
  const firstSearchMs = performance.now() - firstSearchStart;

  const endToEnd = [];
  const native = [];
  let found = 0;
  let dcg = 0;
  for (let i = 0; i < iterations; i++) {
    const id = (i * 997) % files;
    start = performance.now();
    const result = await database.search(`feature_${id} 認証方式`, { limit: 10 });
    endToEnd.push(performance.now() - start);
    native.push(result.elapsedMs);
    const rank = result.hits.findIndex(({ evidence }) => evidence.source.objectId === fileName(id));
    if (rank >= 0) {
      found++;
      dcg += 1 / Math.log2(rank + 2);
    }
  }

  console.log(JSON.stringify({
    platform: process.platform,
    arch: process.arch,
    node: process.versions.node,
    ...indexed,
    iterations,
    firstOpenMs,
    reopenMs,
    firstSearchMs,
    ingestMs,
    searchMs: percentiles(endToEnd),
    rustMs: percentiles(native),
    boundaryAndQueueMs: percentiles(endToEnd.map((ms, i) => Math.max(0, ms - native[i]))),
    queriesPerSecond: 1000 * iterations / endToEnd.reduce((total, ms) => total + ms, 0),
    rssMiB: process.memoryUsage.rss() / 2 ** 20,
    recallAt10: found / iterations,
    ndcgAt10: dcg / iterations,
  }, null, 2));
} finally {
  await database?.close();
  await rm(root, { recursive: true, force: true, maxRetries: 5 });
}
