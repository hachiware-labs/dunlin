import { execFile } from 'node:child_process';
import { mkdtemp, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const counts = process.argv.slice(2).length ? process.argv.slice(2).map(Number) : [1000, 10000];
if (!counts.every((count) => Number.isSafeInteger(count) && count > 0)) throw new Error('文書数には正の整数を指定してください。');
const cli = fileURLToPath(new URL('../tools/rag/cli.mjs', import.meta.url));
const temp = await realpath(tmpdir());
const root = await realpath(await mkdtemp(join(temp, 'dunlin-cli-bench-')));
if (dirname(root) !== temp || !basename(root).startsWith('dunlin-cli-bench-')) throw new Error('Unexpected temporary directory.');
const exec = promisify(execFile);
const env = { ...process.env, DUNLIN_GLOBAL_CONFIG: join(root, 'empty-global.yaml') };
async function run(cwd, ...args) {
  const { stdout } = await exec(process.execPath, [cli, ...args, '--json'], { cwd, env, maxBuffer: 16 * 1024 * 1024 });
  return JSON.parse(stdout).data;
}

try {
  for (const count of counts) {
    const project = join(root, 'project-' + count);
    await run(root, 'init', 'mcp', project);
    const docs = join(project, 'docs');
    for (let start = 0; start < count; start += 100) {
      await Promise.all(Array.from({ length: Math.min(100, count - start) }, (_, offset) => {
        const id = start + offset;
        return writeFile(join(docs, `document-${id}.md`), `# 保全手順\nポンプ ${id} の点検は毎月行う。\n`);
      }));
    }
    const build = await run(project, 'build');
    console.log(JSON.stringify({ files: count, documents: build.documents, elapsed_ms: build.elapsed_ms,
      stages: Object.fromEntries(Object.entries(build.stages).map(([name, stage]) => [name, stage.elapsed_ms])) }));
  }
} finally {
  if (await realpath(root) === root) await rm(root, { recursive: true, force: true, maxRetries: 5 });
}
