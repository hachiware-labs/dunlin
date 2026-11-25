// A project whose KnowledgeNestfile.yaml is written for the versioned store is served by `serve`:
// the index its serve section names (release), over MCP on standard input and output or over
// HTTP, and the next index as soon as `nest release` moves the name, without a restart.

import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import { once } from 'node:events';
import { cp, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { Client } from '@modelcontextprotocol/client';
import { StdioClientTransport } from '@modelcontextprotocol/client/stdio';

const cli = fileURLToPath(new URL('../cli.mjs', import.meta.url));
const fixtures = fileURLToPath(new URL('../../../tests/fixtures/', import.meta.url));
const runNode = promisify(execFile);

async function project(t) {
  const cwd = await mkdtemp(join(tmpdir(), 'knowledge-nest-serve-store-'));
  const cleanup = [];
  t.after(async () => {
    for (const close of cleanup.reverse()) await close();
    await rm(cwd, { recursive: true, force: true, maxRetries: 5 });
  });
  await cp(join(fixtures, 'extract', 'original'), join(cwd, 'docs'), { recursive: true });
  const config = await readFile(join(fixtures, 'optimize', 'KnowledgeNestfile.yaml'), 'utf8');
  await writeFile(join(cwd, 'KnowledgeNestfile.yaml'), config);
  const env = { ...process.env, KNOWLEDGE_NEST_GLOBAL_CONFIG: join(cwd, 'global.yaml') };
  const run = (...args) => runNode(process.execPath, [cli, ...args], { cwd, env, encoding: 'utf8', timeout: 30_000 })
    .catch((error) => error);
  const json = async (...args) => JSON.parse((await run(...args, '--json')).stdout);
  return { cwd, env, run, json, cleanup };
}

async function search(client, query) {
  const output = await client.callTool({ name: 'knowledge.search', arguments: { query } });
  return JSON.parse(output.content[0].text);
}

test('serves the released index of the versioned store over MCP, and the next one after a release', async (t) => {
  const kb = await project(t);

  // Built but not released: nothing is served, and serve says what to do.
  assert.equal((await kb.json('build')).status, 'succeeded');
  const unreleased = await kb.run('serve');
  assert.equal(unreleased.code, 3);
  assert.match(unreleased.stderr, /No index is served as release\. Run nest release <index>, or nest build --release\./);

  // The MCP client is given the command that serves the project on standard input and output.
  const launch = (await kb.json('serve', '--print-config')).data;
  assert.deepEqual(launch.args.slice(1), ['serve', '--project', kb.cwd, '--transport', 'stdio']);

  assert.equal((await kb.json('build', '--release')).status, 'succeeded');
  const client = new Client({ name: 'serve-store-test', version: '1.0.0' });
  kb.cleanup.push(() => client.close());
  await client.connect(new StdioClientTransport({ ...launch, cwd: kb.cwd, env: kb.env, stderr: 'ignore' }));
  assert.deepEqual((await client.listTools()).tools.map(({ name }) => name).sort(), ['knowledge.read', 'knowledge.search']);

  const found = await search(client, 'ポンプの点検');
  assert.ok(found.results.length > 0, JSON.stringify(found));
  assert.ok(found.results.some(({ document_id }) => document_id === 'docs:manuals/pump.md'), JSON.stringify(found.results));
  assert.ok(found.context.text.length > 0);
  const served = found.build_id;

  // A new index is built, but the served one stays until it is released.
  await writeFile(join(kb.cwd, 'docs', 'valve.md'), '# バルブ\n\nバルブの交換はスピンドル交換手順に従う。\n');
  assert.equal((await kb.json('build')).status, 'succeeded');
  const before = await search(client, 'スピンドル交換手順');
  assert.equal(before.build_id, served);
  assert.ok(before.results.every(({ document_id }) => document_id !== 'docs:valve.md'));

  // Released: the next request uses it, with no restart.
  assert.equal((await kb.json('release', 'main')).status, 'succeeded');
  const after = await search(client, 'スピンドル交換手順');
  assert.notEqual(after.build_id, served);
  assert.equal(after.results[0].document_id, 'docs:valve.md');

  // The text of a result is read back from the index served.
  const read = await client.callTool({ name: 'knowledge.read', arguments: { evidence: after.results[0].evidence } });
  assert.match(read.content[0].text, /スピンドル交換手順/);
});

test('release serves a build of a project made by init, as use did', async (t) => {
  const cwd = await mkdtemp(join(tmpdir(), 'knowledge-nest-release-init-'));
  t.after(() => rm(cwd, { recursive: true, force: true, maxRetries: 5 }));
  const env = { ...process.env, KNOWLEDGE_NEST_GLOBAL_CONFIG: join(cwd, 'global.yaml') };
  const run = (...args) => runNode(process.execPath, [cli, ...args], { cwd, env, encoding: 'utf8', timeout: 30_000 })
    .catch((error) => error);
  const json = async (...args) => JSON.parse((await run(...args, '--json')).stdout);
  await cp(join(fixtures, 'extract', 'original'), join(cwd, 'manuals'), { recursive: true });
  assert.equal((await json('init', 'mcp', 'kb', '--source', 'manuals')).status, 'succeeded');
  const kb = (...args) => json('--project', 'kb', ...args);
  const first = (await kb('build', '--tag', 'first')).data;

  // A candidate built from another configuration is not served until it is released.
  const configFile = join(cwd, 'kb', 'KnowledgeNestfile.yaml');
  const candidate = join(cwd, 'kb', 'candidate.yaml');
  await writeFile(candidate, (await readFile(configFile, 'utf8')).replace('size: 800', 'size: 300'));
  const built = (await kb('build', '--from', 'transform', '--input', first.build_id, '--config', candidate, '--tag', 'cand')).data;
  assert.notEqual(built.build_id, first.build_id);
  assert.match((await run('--project', 'kb', 'build', '--from', 'transform', '--input', first.build_id, '--config', candidate)).stdout, /Run nest release /);

  const released = await kb('release', 'cand');
  assert.equal(released.status, 'succeeded', JSON.stringify(released.errors));
  assert.equal(released.command, 'release');
  assert.equal((await kb('search', 'ポンプ')).data.build_id, built.build_id);

  // A project made by init serves one build, so a served name does not apply.
  const named = await run('--project', 'kb', 'release', 'first', '--name', 'other');
  assert.equal(named.code, 2);
  assert.match(named.stderr, /--name works with a configuration file written as lists of modules/);
  // The earlier name still works.
  assert.equal((await kb('use', 'first')).status, 'succeeded');
  assert.equal((await kb('search', 'ポンプ')).data.build_id, first.build_id);
});

test('serves the search of the versioned store over HTTP, without the answer endpoint', async (t) => {
  const kb = await project(t);
  assert.equal((await kb.json('build', '--release')).status, 'succeeded');

  // The answer endpoint needs an answer model, which this form does not have yet.
  const config = await readFile(join(kb.cwd, 'KnowledgeNestfile.yaml'), 'utf8');
  await writeFile(join(kb.cwd, 'KnowledgeNestfile.yaml'), config.replace('  tag: release', '  tag: release\n  transport: http\n  endpoints: [responses, search]'));
  const refused = await kb.run('serve');
  assert.equal(refused.code, 2);
  assert.match(refused.stderr, /The responses endpoint needs an answer model/);
  await writeFile(join(kb.cwd, 'KnowledgeNestfile.yaml'), config);

  const child = spawn(process.execPath, [cli, 'serve', '--transport', 'http', '--port', '0'], { cwd: kb.cwd, env: kb.env, stdio: ['ignore', 'ignore', 'pipe'] });
  const exited = once(child, 'exit');
  kb.cleanup.push(async () => {
    if (child.exitCode === null && child.signalCode === null) child.kill();
    await exited;
  });
  const base = await new Promise((resolve, reject) => {
    let log = '';
    child.stderr.setEncoding('utf8').on('data', (chunk) => {
      log += chunk;
      const address = /^Knowledge-Nest: (http:\/\/[^\s]+)$/mu.exec(log)?.[1];
      if (address) resolve(new URL(address));
    });
    exited.then(([code]) => reject(new Error(`serve exited with code ${code}\n${log}`)), reject);
  });
  const post = (path, body) => fetch(new URL(path, base), { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });
  const searched = await post('/v1/search', { query: 'ポンプの点検' });
  assert.equal(searched.status, 200);
  assert.ok((await searched.json()).results.some(({ document_id }) => document_id === 'docs:manuals/pump.md'));
  assert.equal((await post('/v1/responses', { input: 'ポンプの点検は？' })).status, 404);
});
