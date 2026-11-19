// Where a build's bytes go: the corpus revisions, the chunk sets (and their `searchText`), the
// originals and each build's keyword index. Builds a project from the Markdown files in the given
// folders (default: docs/).
import { execFile } from 'node:child_process';
import { cp, mkdtemp, readdir, realpath, rm, stat } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, dirname, join } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const folders = process.argv.slice(2).length ? process.argv.slice(2) : [fileURLToPath(new URL('../docs', import.meta.url))];
const cli = fileURLToPath(new URL('../tools/rag/cli.mjs', import.meta.url));
const temp = await realpath(tmpdir());
const root = await realpath(await mkdtemp(join(temp, 'dunlin-storage-bench-')));
if (dirname(root) !== temp || !basename(root).startsWith('dunlin-storage-bench-')) throw new Error('Unexpected temporary directory.');
const exec = promisify(execFile);
const env = { ...process.env, DUNLIN_GLOBAL_CONFIG: join(root, 'empty-global.yaml') };
async function run(cwd, ...args) {
  const { stdout } = await exec(process.execPath, [cli, ...args, '--json'], { cwd, env, maxBuffer: 64 * 1024 * 1024 });
  return JSON.parse(stdout).data;
}
async function bytes(path) {
  const entry = await stat(path).catch(() => null);
  if (!entry) return 0;
  if (!entry.isDirectory()) return entry.size;
  let total = 0;
  for (const name of await readdir(path)) total += await bytes(join(path, name));
  return total;
}
async function find(path, name) {
  for (const entry of await readdir(path, { withFileTypes: true })) {
    const child = join(path, entry.name);
    if (entry.name === name) return child;
    if (entry.isDirectory()) {
      const found = await find(child, name);
      if (found) return found;
    }
  }
  return null;
}

try {
  const project = join(root, 'project');
  await run(root, 'init', 'mcp', project);
  const docs = join(project, 'docs');
  for (const folder of folders) await cp(folder, join(docs, basename(folder)), { recursive: true, filter: (path) => !/\.(png|jpe?g|svg)$/iu.test(path) });
  const build = await run(project, 'build');
  const data = join(project, '.dunlin');
  const corpus = new DatabaseSync(await find(data, 'corpus.sqlite'), { readOnly: true });
  let chunkBytes = 0;
  let searchTextBytes = 0;
  let rebuilt = 0;
  let chunkCount = 0;
  for (const { chunks } of corpus.prepare('SELECT chunks FROM chunk_sets').iterate()) {
    chunkBytes += Buffer.byteLength(chunks);
    for (const chunk of JSON.parse(chunks)) {
      chunkCount++;
      if (chunk.searchText === true) rebuilt++;
      else if (chunk.searchText !== undefined) searchTextBytes += Buffer.byteLength(JSON.stringify(chunk.searchText)) + '"searchText":,'.length;
    }
  }
  const revisions = corpus.prepare('SELECT count(*) AS count, sum(length(record)) AS bytes FROM revisions').get();
  corpus.close();
  const originals = await find(data, 'originals');
  const index = await find(data, '.dunlin-index-v1');
  const store = index ? (await readdir(index)).filter((name) => name.endsWith('.store')) : [];
  let storeBytes = 0;
  for (const name of store) storeBytes += await bytes(join(index, name));
  console.log(JSON.stringify({
    documents: build.documents,
    elapsed_ms: build.elapsed_ms,
    source_bytes: await bytes(docs),
    originals_bytes: originals ? await bytes(originals) : 0,
    corpus_revisions: revisions,
    chunks: chunkCount,
    chunk_sets_bytes: chunkBytes,
    search_text_bytes: searchTextBytes,
    search_text_rebuilt_on_read: rebuilt,
    index_bytes: index ? await bytes(index) : 0,
    index_store_bytes: storeBytes,
  }, null, 2));
} finally {
  if (await realpath(root) === root) await rm(root, { recursive: true, force: true, maxRetries: 5 });
}
