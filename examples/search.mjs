import { KnowledgeNest } from 'knowledge-nest';

const [path, query = 'OAuth'] = process.argv.slice(2);
if (!path) {
  console.error('Usage: node search.mjs <source-path> [keyword]');
  process.exitCode = 1;
} else {
  const database = await KnowledgeNest.open('./knowledge');
  try {
    await database.addSource({ id: 'docs', kind: 'filesystem', path });
    const { hits } = await database.search(query, { limit: 5 });
    for (const { evidence } of hits) {
      const result = await database.read(evidence);
      console.log(result.evidence.source.url, result.evidence.range, result.content);
    }
  } finally {
    await database.close();
  }
}
