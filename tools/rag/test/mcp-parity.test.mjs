// Spec 28.10 step 3c: the Rust MCP server over stdio publishes the tools of the former JavaScript
// server (test/fixtures/mcp-reference.mjs) and answers the same requests in the same way.
import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { DatabaseSync } from 'node:sqlite';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import { currentSearchSchema } from './fixtures/search-schema.mjs';
import { promisify } from 'node:util';
import { parse, stringify } from 'yaml';

const cli = fileURLToPath(new URL('../cli.mjs', import.meta.url));
const example = fileURLToPath(new URL('../../../examples/mcp.mjs', import.meta.url));
const reference = fileURLToPath(new URL('./fixtures/mcp-reference-serve.mjs', import.meta.url));
const runNode = promisify(execFile);

/** Starts a stdio server; `send` writes a message and `answer(id)` waits for the response to it. */
function server(t, args, cwd, env) {
  const child = spawn(process.execPath, args, { cwd, env, stdio: ['pipe', 'pipe', 'ignore'] });
  const answers = new Map();
  let buffer = '';
  child.stdout.setEncoding('utf8');
  child.stdout.on('data', (chunk) => {
    buffer += chunk;
    let at;
    while ((at = buffer.indexOf('\n')) >= 0) {
      const message = JSON.parse(buffer.slice(0, at));
      buffer = buffer.slice(at + 1);
      if (message.id !== undefined) answers.set(message.id, message);
    }
  });
  const closed = new Promise((resolve) => child.once('exit', resolve));
  t.after(async () => {
    child.stdin.end();
    await Promise.race([closed, new Promise((resolve) => setTimeout(resolve, 3000))]);
    child.kill();
  });
  return {
    send: (message) => child.stdin.write(JSON.stringify({ jsonrpc: '2.0', ...message }) + '\n'),
    async answer(id) {
      const deadline = Date.now() + 20_000;
      while (!answers.has(id)) {
        if (Date.now() > deadline) throw new Error('No answer to request ' + id);
        await new Promise((resolve) => setTimeout(resolve, 10));
      }
      return answers.get(id);
    },
    closed,
  };
}

/** A result without the time a search took, in its text and its structured content. */
function timeless(result) {
  if (!result?.content) return result;
  const strip = (value) => value && typeof value === 'object' && 'elapsedMs' in value ? { ...value, elapsedMs: 0 } : value;
  return { ...result, content: result.content.map((item) => item.type === 'text' && item.text.includes('"elapsedMs"')
    ? { ...item, text: item.text.replace(/"elapsedMs":[^,}]+/u, '"elapsedMs":0') } : item),
  ...(result.structuredContent && { structuredContent: strip(result.structuredContent) }) };
}

/** Sends the same requests to both servers and checks that the answers agree. */
async function compare(t, actual, expected) {
  let id = 0;
  const both = async (method, params) => {
    id++;
    actual.send({ id, method, params });
    expected.send({ id, method, params });
    const [a, b] = await Promise.all([actual.answer(id), expected.answer(id)]);
    assert.deepEqual(a.error, b.error, JSON.stringify({ method, params }));
    assert.deepEqual(timeless(a.result), timeless(currentSearchSchema(b.result)), JSON.stringify({ method, params }));
    return a.result;
  };
  for (const version of ['2025-11-25']) {
    id++;
    const params = { protocolVersion: version, capabilities: {}, clientInfo: { name: 'parity', version: '1' } };
    actual.send({ id, method: 'initialize', params });
    expected.send({ id, method: 'initialize', params });
    const [a, b] = await Promise.all([actual.answer(id), expected.answer(id)]);
    for (const key of ['protocolVersion', 'capabilities', 'serverInfo']) assert.deepEqual(a.result[key], b.result[key], key);
  }
  for (const side of [actual, expected]) side.send({ method: 'notifications/initialized' });
  const { tools } = await both('tools/list', {});
  const call = (name, args) => both('tools/call', { name, arguments: args });
  return { tools, call, both };
}

async function project(t) {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-mcp-parity-'));
  const env = { ...process.env, DUNLIN_GLOBAL_CONFIG: join(root, 'global.yaml') };
  await runNode(process.execPath, [cli, 'init', 'mcp', 'kb'], { cwd: root, env });
  // The Node API example opens an index with the default keyword settings: words.
  const configPath = join(root, 'kb', 'Dunlinfile.yaml');
  const config = parse(await readFile(configPath, 'utf8'));
  config.index.modules[0].with = { tokenizer: 'word', short_query: 'none' };
  await writeFile(configPath, stringify(config));
  await writeFile(join(root, 'kb', 'docs', 'guide.md'), '# Steps\n\nReplace the pump seal. See [notes](notes.md).\n\n## Checks\n\nCheck the pump monthly.\n');
  await writeFile(join(root, 'kb', 'docs', 'notes.md'), '# Notes\n\nThe pump was replaced in April.\n');
  await runNode(process.execPath, [cli, '--project', 'kb', 'build'], { cwd: root, env });
  const state = new DatabaseSync(join(root, 'kb', '.dunlin', 'state.sqlite'), { readOnly: true });
  const build = JSON.parse(state.prepare('SELECT manifest FROM project JOIN builds ON builds.id = project.build_id').get().manifest);
  state.close();
  return { root, env, index: join(root, 'kb', '.dunlin', 'artifacts', build.index) };
}

const invalidSearches = [
  { query: 'x', limit: 0, budgetChars: 2000000 }, { query: 'x', sourceIds: ['ok', 'bad id'] }, { query: 5 }, {},
  { query: 'x', limit: 1.5 }, { query: 'x', limit: '3' }, { query: 'x', sourceIds: [] }, { query: 'x'.repeat(5000) },
];

async function reads(call, evidence) {
  await call('knowledge.read', { evidence });
  await call('knowledge.read', { evidence: { ...evidence, range: { startLine: 1, endLine: 999 } } });
  await call('knowledge.read', { evidence: { ...evidence, range: { startLine: 2 ** 20 + 1, endLine: 2 ** 20 + 2 } } });
  await call('knowledge.read', { evidence: { source: {}, range: { startLine: 0 } } });
  await call('knowledge.read', {});
}

test('serves an index over stdio like the former Node API server', { timeout: 120_000 }, async (t) => {
  const { root, env, index } = await project(t);
  const actual = server(t, [example, index], root, env);
  const expected = server(t, [reference, 'library', index], root, env);
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const { tools, call, both } = await compare(t, actual, expected);
  assert.deepEqual(tools.map(({ name }) => name), ['knowledge.search', 'knowledge.read', 'knowledge.explore']);
  const found = await call('knowledge.search', { query: 'pump', limit: 3 });
  assert.ok(found.structuredContent.hits.length > 0);
  await call('knowledge.search', { query: 'pump', sourceIds: ['docs'], unknown: true });
  for (const args of invalidSearches) await call('knowledge.search', args);
  await reads(call, found.structuredContent.hits[0].evidence);
  await call('knowledge.explore', { query: 'pump', maxSteps: 4 });
  await call('knowledge.explore', { query: 'pump', timeoutMs: 0 });
  await call('knowledge.missing', {});
  await both('ping', {});
});

test('serves a project over stdio like the former project server', { timeout: 120_000 }, async (t) => {
  const { root, env } = await project(t);
  const actual = server(t, [cli, '--project', 'kb', 'serve'], root, env);
  const expected = server(t, [reference, 'project', join(root, 'kb')], root, env);
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const { tools, call } = await compare(t, actual, expected);
  assert.deepEqual(tools.map(({ name }) => name), ['knowledge.search', 'knowledge.read']);
  const found = await call('knowledge.search', { query: 'pump' });
  assert.ok(found.structuredContent.results.length > 0);
  await call('knowledge.search', { query: 'pump', limit: 1, budget_chars: 50, source_ids: ['docs'] });
  await call('knowledge.search', { query: 'pump', budgetChars: 50, budget_chars: 50 });
  await call('knowledge.search', { query: '   ' });
  for (const args of invalidSearches) await call('knowledge.search', args);
  await reads(call, found.structuredContent.results[0].evidence);
});
