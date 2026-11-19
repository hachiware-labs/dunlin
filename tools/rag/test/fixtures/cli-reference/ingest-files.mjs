import { opendir } from 'node:fs/promises';
import { extname, join } from 'node:path';

const textExtensions = new Set(['.md', '.markdown', '.txt', '.rs', '.js', '.mjs', '.cjs', '.ts', '.tsx', '.jsx',
  '.py', '.go', '.java', '.cs', '.json', '.toml', '.yaml', '.yml', '.sql']);

export function isPlainDocument(path) {
  return textExtensions.has(extname(path).toLowerCase());
}

/** Lists the files under a folder, skipping names that start with a dot, node_modules and links. */
export async function* listFiles(path) {
  const directory = await opendir(path);
  for await (const entry of directory) {
    if (entry.name.startsWith('.') || entry.name === 'node_modules' || entry.isSymbolicLink()) continue;
    const file = join(path, entry.name);
    if (entry.isDirectory()) yield* listFiles(file);
    else if (entry.isFile()) yield file;
  }
}
