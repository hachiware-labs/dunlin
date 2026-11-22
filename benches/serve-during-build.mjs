// Search latency of `nest serve` while a build adds documents to the shared corpus. Builds a
// project, serves it over HTTP, sends searches one after another, then adds documents and builds
// again. Prints the latency before, during and after that build.
//
//   node benches/serve-during-build.mjs [documents] [added documents] [characters per document]
import { execFile, spawn } from 'node:child_process';
import { mkdir, mkdtemp, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, dirname, join } from 'node:path';
import { performance } from 'node:perf_hooks';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const [initial = 2000, added = 3000, size = 16000] = process.argv.slice(2).map(Number);
const cli = fileURLToPath(new URL('../tools/rag/cli.mjs', import.meta.url));
const temp = await realpath(tmpdir());
const root = await realpath(await mkdtemp(join(temp, 'knowledge-nest-serve-bench-')));
if (dirname(root) !== temp || !basename(root).startsWith('knowledge-nest-serve-bench-')) throw new Error('Unexpected temporary directory.');
const exec = promisify(execFile);
const env = { ...process.env, KNOWLEDGE_NEST_GLOBAL_CONFIG: join(root, 'empty-global.yaml') };
const port = 40000 + Math.floor(Math.random() * 20000);

function document(id) {
  const lines = [`# 設備 ${id} の保全手順`];
  for (let j = 0; lines.join('\n').length < size; j++) {
    lines.push(`## 点検 ${j}`, `ポンプ${id % 97} とバルブ${j % 31} を点検し、圧力計${(id + j) % 53} の値を記録する。異常があれば交換手順${j % 17} に従う。`);
  }
  return lines.join('\n') + '\n';
}

async function write(docs, from, count) {
  for (let start = from; start < from + count; start += 200) {
    await Promise.all(Array.from({ length: Math.min(200, from + count - start) }, (_, offset) =>
      writeFile(join(docs, `equipment-${start + offset}.md`), document(start + offset))));
  }
}

function summary(samples) {
  if (!samples.length) return null;
  const sorted = [...samples].sort((a, b) => a - b);
  const at = (q) => Number(sorted[Math.min(sorted.length - 1, Math.ceil(q * sorted.length) - 1)].toFixed(1));
  return { requests: samples.length, p50: at(0.5), p95: at(0.95), p99: at(0.99), max: Number(sorted.at(-1).toFixed(1)) };
}

let server;
try {
  const project = join(root, 'project');
  await exec(process.execPath, [cli, 'init', 'rag', project, '--json'], { cwd: root, env });
  const docs = join(project, 'docs');
  await mkdir(docs, { recursive: true });
  await write(docs, 0, initial);
  await exec(process.execPath, [cli, 'build', '--json'], { cwd: project, env, maxBuffer: 64 * 1024 * 1024 });

  server = spawn(process.execPath, [cli, 'serve', '--port', String(port)], { cwd: project, env, stdio: ['ignore', 'ignore', 'pipe'] });
  let errors = '';
  server.stderr.on('data', (chunk) => { errors += chunk; });
  const url = `http://127.0.0.1:${port}/v1/search`;
  let query = 0;
  async function search() {
    const body = JSON.stringify({ query: `ポンプ${query % 97} 圧力計${(query * 7) % 53} 交換手順`, limit: 10 });
    query++;
    const start = performance.now();
    const response = await fetch(url, { method: 'POST', headers: { 'content-type': 'application/json' }, body });
    await response.text();
    return { ms: performance.now() - start, ok: response.ok, status: response.status };
  }
  for (let attempt = 0; ; attempt++) {
    try {
      if ((await search()).ok) break;
    } catch {}
    if (attempt > 300) throw new Error(`serve did not start: ${errors}`);
    await delay(100);
  }

  const phases = { before: [], during: [], after: [] };
  const failures = { before: 0, during: 0, after: 0 };
  let phase = 'before';
  let running = true;
  const load = (async () => {
    while (running) {
      const result = await search();
      if (result.ok) phases[phase].push(result.ms);
      else failures[phase]++;
    }
  })();
  await delay(5000);
  await write(docs, initial, added);
  phase = 'during';
  const started = performance.now();
  const { stdout } = await exec(process.execPath, [cli, 'build', '--json'], { cwd: project, env, maxBuffer: 64 * 1024 * 1024 });
  const buildMs = performance.now() - started;
  phase = 'after';
  await delay(5000);
  running = false;
  await load;
  const build = JSON.parse(stdout).data;
  console.log(JSON.stringify({
    documents: initial,
    added,
    characters: size,
    build_ms: Math.round(buildMs),
    stages: Object.fromEntries(Object.entries(build.stages).map(([name, stage]) => [name, stage.elapsed_ms])),
    latency_ms: Object.fromEntries(Object.entries(phases).map(([name, samples]) => [name, summary(samples)])),
    failures,
  }));
} finally {
  server?.kill();
  await delay(500);
  if (await realpath(root) === root) await rm(root, { recursive: true, force: true, maxRetries: 10 });
}
