import { Dunlin } from 'dunlin';

const directory = process.argv[2];
if (!directory) {
  console.error('Usage: node mcp.mjs <index-directory>');
  process.exitCode = 1;
} else {
  const database = await Dunlin.open(directory);
  await database.serveMcp({ transport: 'stdio' });
}
