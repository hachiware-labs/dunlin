import { KnowledgeNest } from 'knowledge-nest';

const directory = process.argv[2];
if (!directory) {
  console.error('Usage: node mcp.mjs <index-directory>');
  process.exitCode = 1;
} else {
  const database = await KnowledgeNest.open(directory);
  await database.serveMcp({ transport: 'stdio' });
}
