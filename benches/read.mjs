// Search then read the top hits, as context assembly does. Indexes the Markdown files in the given
// folder (default: docs/), or `count` generated files with `--generate count`.
import { cp, mkdtemp, rm, writeFile, mkdir } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { performance } from 'node:perf_hooks';
import { fileURLToPath } from 'node:url';
import { Dunlin } from 'dunlin';

const generate = process.argv[2] === '--generate' ? Number(process.argv[3]) : 0;
const folder = generate ? null : process.argv[2] ?? fileURLToPath(new URL('../docs', import.meta.url));
const queries = generate
  ? Array.from({ length: 50 }, (_, i) => `feature_${(i * 997) % generate} 認証方式`)
  : ['検索', '評価 レポート', 'チャンク', '原本', 'ビルド タグ', 'Wiki ページ', 'MCP サーバー', '辞書', '索引', '設定ファイル'];
const rounds = 20;

function percentiles(samples) {
  const sorted = [...samples].sort((a, b) => a - b);
  const quantile = (q) => Number(sorted[Math.ceil(q * sorted.length) - 1].toFixed(3));
  return { p50: quantile(0.5), p95: quantile(0.95), mean: Number((samples.reduce((a, b) => a + b, 0) / samples.length).toFixed(3)) };
}

const root = await mkdtemp(join(tmpdir(), 'dunlin-read-bench-'));
let database;
try {
  const docs = join(root, 'docs');
  if (generate) {
    await mkdir(docs);
    for (let id = 0; id < generate; id++) {
      await writeFile(join(docs, `document-${id}.md`), `# 認証方式\nOAuth sample feature_${id}.\n## 変更履歴\n設計を見直した。`);
    }
  } else {
    await cp(folder, docs, { recursive: true, filter: (path) => !/\.(png|jpe?g|svg)$/iu.test(path) });
  }
  database = await Dunlin.open(join(root, 'index'));
  await database.addSource({ id: 'bench', kind: 'filesystem', path: docs });
  const search = [];
  const read = [];
  let reads = 0;
  for (let round = 0; round < rounds; round++) {
    for (const query of queries) {
      let start = performance.now();
      const { hits } = await database.search(query, { limit: 10 });
      search.push(performance.now() - start);
      start = performance.now();
      for (const hit of hits) await database.read(hit.evidence);
      reads += hits.length;
      if (hits.length) read.push(performance.now() - start);
    }
  }
  console.log(JSON.stringify({ reads, search_ms: percentiles(search), read_top10_ms: percentiles(read) }));
} finally {
  await database?.close();
  await rm(root, { recursive: true, force: true, maxRetries: 5 });
}
