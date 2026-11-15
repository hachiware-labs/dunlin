import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { createServer, request } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { Client, StreamableHTTPClientTransport } from '@modelcontextprotocol/client';
import { StdioClientTransport } from '@modelcontextprotocol/client/stdio';
import { parse, stringify } from 'yaml';

const cli = fileURLToPath(new URL('../cli.mjs', import.meta.url));
const runNode = promisify(execFile);
const query = '交換周期';
const content = '🧰 点検の案内です。\n交換周期は6か月です。\n';

async function project(t) {
  const cwd = await mkdtemp(join(tmpdir(), 'dunlin-responses-'));
  const cleanup = [];
  t.after(async () => {
    for (const close of cleanup.reverse()) await close();
    await rm(cwd, { recursive: true, force: true, maxRetries: 5 });
  });
  const env = { ...process.env, DUNLIN_GLOBAL_CONFIG: join(cwd, 'global.yaml'), DUNLIN_TEST_TOKEN: '' };
  const run = (...args) => runNode(process.execPath, [cli, ...args], { cwd, env, encoding: 'utf8', timeout: 15_000 });
  const json = async (...args) => JSON.parse((await run(...args, '--json')).stdout);
  await run('init', 'rag', '.');
  const document = join(cwd, 'docs', 'guide.txt');
  await writeFile(document, content);
  const build = (await json('build')).data;
  const configFile = join(cwd, 'Dunlinfile.yaml');
  const config = parse(await readFile(configFile, 'utf8'));
  const save = () => writeFile(configFile, stringify(config));
  return { cwd, env, run, json, document, config, configFile, save, build, cleanup };
}

async function start({ cwd, env, cleanup }, ...args) {
  const child = spawn(process.execPath, [cli, 'serve', ...args], { cwd, env, stdio: ['ignore', 'ignore', 'pipe'] });
  const exited = once(child, 'exit');
  cleanup.push(async () => {
    if (child.exitCode === null && child.signalCode === null) child.kill();
    await exited;
  });
  return new Promise((resolve, reject) => {
    let log = '';
    child.stderr.setEncoding('utf8').on('data', (chunk) => {
      log += chunk;
      const address = /^Dunlin rag: (http:\/\/[^\s]+)$/mu.exec(log)?.[1];
      if (address) resolve(new URL(address));
    });
    exited.then(([code]) => reject(new Error('serve exited with code ' + code + '\n' + log)), reject);
  });
}

function post(base, path, body, headers) {
  return fetch(new URL(path, base), { method: 'POST', headers: { 'content-type': 'application/json', ...headers },
    body: JSON.stringify(body), signal: AbortSignal.timeout(10000) });
}

function rawSearch(base, headers, chunks) {
  return new Promise((resolve, reject) => {
    const outgoing = request(new URL('/v1/search', base), { method: 'POST',
      headers: { 'content-type': 'application/json', ...headers } }, async (response) => {
      try {
        const output = [];
        for await (const chunk of response) output.push(chunk);
        resolve(new Response(Buffer.concat(output), { status: response.statusCode }));
      } catch (error) { reject(error); }
    });
    outgoing.on('error', reject);
    outgoing.setTimeout(10000, () => outgoing.destroy(new Error('Request timed out.')));
    for (const chunk of chunks) outgoing.write(chunk);
    outgoing.end();
  });
}

async function ok(response) {
  const result = await response;
  const body = await result.json();
  assert.equal(result.status, 200, JSON.stringify(body));
  return body;
}

async function error(response, status, code) {
  const result = await response;
  const body = await result.json();
  assert.equal(result.status, status, JSON.stringify(body));
  assert.equal(body.error.code, code);
}

async function mockModel(kb) {
  const calls = [];
  const server = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const { messages } = JSON.parse(Buffer.concat(chunks));
    const input = JSON.parse(messages[1].content);
    calls.push(input);
    if (input.question.includes('待機')) await delay(2500);
    const facts = [{ text: input.evidence[0].text.trim(), evidence_ids: [input.question.includes('不正') ? 999 : 1] }];
    response.setHeader('content-type', 'application/json');
    response.end(JSON.stringify({ done: true, prompt_eval_count: 25, eval_count: 10,
      message: { content: JSON.stringify({ facts }) } }));
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  kb.cleanup.push(async () => {
    server.closeAllConnections();
    await new Promise((resolve) => server.close(resolve));
  });
  return { url: 'http://127.0.0.1:' + server.address().port, calls };
}

function checkAnswer(response, search) {
  assert.equal(response.object, 'response');
  assert.equal(response.status, 'completed');
  assert.equal(response.model, 'local-test');
  assert.deepEqual(response.metadata, { dunlin_build_id: search.build_id, dunlin_search_config_id: search.search_config_id });
  const [answer] = response.output[0].content;
  assert.match(answer.text, /交換周期/u);
  assert.equal(response.usage.total_tokens, 35);
  assert.equal(answer.annotations[0].file_id, 'docs:guide.txt');
  const source = search.context.segments.find(({ kind }) => kind === 'body');
  assert.deepEqual(response.dunlin_citations[0], { annotation_index: 0, document_id: source.document_id,
    document_version: source.document_version, source_range: source.source_range });
}

test('shares one RAG search payload, grounded answers and live index across CLI, MCP, and HTTP', { timeout: 45_000 }, async (t) => {
  const kb = await project(t);
  await assert.rejects(kb.json('ask', query), (error) => error.code === 3 && JSON.parse(error.stdout).errors[0].code === 'MODEL_REQUIRED');
  const withoutModel = await start(kb, '--port', '0');
  assert.equal((await ok(post(withoutModel, '/v1/search', { query }))).build_id, kb.build.build_id);
  await error(post(withoutModel, '/v1/responses', { input: query }), 503, 'MODEL_REQUIRED');
  const model = await mockModel(kb);
  Object.assign(kb.config.models.answerer, { model: 'local-test', base_url: model.url });
  await kb.save();
  const launch = (await kb.json('serve', '--print-config')).data;
  assert.equal(launch.url, 'http://127.0.0.1:4318/mcp');
  const stdio = new Client({ name: 'stdio-test', version: '1.0.0' });
  const stdioLaunch = (await kb.json('serve', '--transport', 'stdio', '--print-config')).data;
  kb.cleanup.push(() => stdio.close());
  await stdio.connect(new StdioClientTransport({ ...stdioLaunch, cwd: kb.cwd, stderr: 'ignore' }));
  const base = await start(kb, '--port', '0');
  const mcp = new Client({ name: 'http-test', version: '1.0.0' });
  kb.cleanup.push(() => mcp.close());
  await mcp.connect(new StreamableHTTPClientTransport(new URL('/mcp', base)));
  assert.deepEqual((await mcp.listTools()).tools.map(({ name }) => name), ['knowledge.search']);
  const mcpSearch = async (client, args = {}) => {
    const output = await client.callTool({ name: 'knowledge.search', arguments: { query, ...args } });
    assert.ok(!output.isError, JSON.stringify(output));
    return output.structuredContent;
  };

  const cliSearch = (await kb.json('search', query)).data;
  assert.deepEqual(await ok(post(base, '/v1/search', { query })), cliSearch);
  assert.deepEqual(await mcpSearch(mcp), cliSearch);
  assert.deepEqual(await mcpSearch(stdio), cliSearch);
  assert.equal(cliSearch.schema_version, 1);
  assert.equal(cliSearch.build_id, kb.build.build_id);
  const chars = [...cliSearch.context.text];
  assert.equal(chars.length, cliSearch.context.chars);
  for (const { output_range, source_range, kind } of cliSearch.context.segments) {
    if (kind === 'body') assert.equal(chars.slice(...output_range).join(''), [...content].slice(...source_range).join(''));
  }
  const budget = cliSearch.context.segments.find(({ kind }) => kind === 'body').output_range[0] + 5;
  const overrides = (await kb.json('search', query, '--budget-chars', String(budget), '--limit', '1')).data;
  assert.deepEqual(await mcpSearch(mcp, { budget_chars: budget, limit: 1 }), overrides);
  assert.deepEqual(await ok(post(base, '/v1/search', { query, budget_chars: budget, limit: 1 })), overrides);
  assert.notEqual(overrides.search_config_id, cliSearch.search_config_id);
  assert.equal(overrides.warnings[0].code, 'SEARCH_OVERRIDES');
  assert.equal(overrides.context.chars, budget);
  assert.deepEqual(await mcpSearch(mcp), cliSearch);

  checkAnswer((await kb.json('ask', query)).data, cliSearch);
  checkAnswer(await ok(post(base, '/v1/responses', { input: [{ role: 'user', content: '前の質問' },
    { role: 'assistant', content: '省略' }, { role: 'user', content: [{ type: 'input_text', text: query }] }],
  model: 'local-test', stream: false })), cliSearch);
  assert.deepEqual(model.calls.map(({ question }) => question), [query, query]);
  assert.deepEqual(model.calls[0], model.calls[1]);
  assert.match((await kb.run('ask', query)).stdout, /docs:guide\.txt/u);
  const decline = await ok(post(base, '/v1/responses', { input: 'xyzzy' }));
  assert.match(decline.output[0].content[0].text, /根拠を確認できませんでした/u);
  assert.deepEqual(decline.output[0].content[0].annotations, []);
  assert.equal(model.calls.length, 3);
  for (const unsupported of [{ stream: true }, { previous_response_id: 'resp_previous' }, { tools: [] }, { background: true }]) {
    await error(post(base, '/v1/responses', { input: query, ...unsupported }), 400, 'INVALID_REQUEST');
  }
  await error(post(base, '/v1/responses', { input: query, model: 'other' }), 400, 'MODEL_MISMATCH');
  await error(post(base, '/v1/responses', { input: query + ' 不正' }), 500, 'INVALID_CITATION');

  await writeFile(kb.document, content.replace('6か月', '3か月'));
  await kb.json('build');
  const updated = (await kb.json('search', query)).data;
  assert.notEqual(updated.build_id, cliSearch.build_id);
  assert.deepEqual(await ok(post(base, '/v1/search', { query })), updated);
  assert.deepEqual(await mcpSearch(mcp), updated);
  checkAnswer(await ok(post(base, '/v1/responses', { input: query })), updated);
});

test('enforces host, origin, token, endpoint, size, time and concurrency limits on a live listener', { timeout: 45_000 }, async (t) => {
  const kb = await project(t);
  const model = await mockModel(kb);
  Object.assign(kb.config.models.answerer, { model: 'local-test', base_url: model.url });
  kb.config.serve.host = '0.0.0.0';
  await kb.save();
  await assert.rejects(kb.json('validate'), (error) => error.code === 2 && JSON.parse(error.stdout).errors[0].code === 'ARG_CONFLICT');
  await assert.rejects(kb.run('serve'), { code: 2 });
  kb.config.serve.host = '127.0.0.1';
  const allowed = 'https://reader.example';
  kb.config.serve.access.allowed_origins = [allowed];
  kb.config.serve.access.auth = { type: 'token', token_env: 'DUNLIN_TEST_TOKEN' };
  kb.config.serve.limits = { max_request_bytes: 450, request_timeout_seconds: 2, max_concurrent: 1 };
  kb.config.serve.endpoints = ['search', 'responses'];
  await kb.save();
  await assert.rejects(kb.run('serve', '--port', '0'), { code: 2, stderr: /environment variable/u });
  await assert.rejects(kb.run('serve', '--transport', 'stdio', '--port', '0'), { code: 2 });
  await assert.rejects(kb.run('serve', '--print-config'), { code: 2 });
  kb.env.DUNLIN_TEST_TOKEN = 'local-test-token';
  const base = await start(kb, '--host', '0.0.0.0', '--port', '0');
  assert.equal(base.hostname, '0.0.0.0');
  base.hostname = '127.0.0.1';
  await assert.rejects(kb.run('serve', '--host', '0.0.0.0', '--port', base.port), { code: 1, stderr: /EADDRINUSE/u });
  const auth = { authorization: 'Bearer local-test-token' };
  await error(post(base, '/v1/search', { query }), 401, 'AUTH_REQUIRED');
  await error(post(base, '/v1/search', { query }, { authorization: 'Bearer incorrect' }), 401, 'AUTH_REQUIRED');
  await error(rawSearch(base, { ...auth, host: 'unexpected.example' }, [JSON.stringify({ query })]), 403, 'HOST_FORBIDDEN');
  await error(post(base, '/v1/search', { query }, { ...auth, origin: 'https://unlisted.example' }), 403, 'ORIGIN_FORBIDDEN');
  const permitted = await post(base, '/v1/search', { query }, { ...auth, origin: allowed });
  assert.equal(permitted.headers.get('access-control-allow-origin'), allowed);
  assert.equal((await ok(permitted)).build_id, kb.build.build_id);
  await error(post(base, '/mcp', {}, auth), 404, 'NOT_FOUND');
  await error(post(base, '/v1/search', { query: 'x'.repeat(451) }, auth), 413, 'BODY_LIMIT');
  await error(rawSearch(base, { ...auth, 'transfer-encoding': 'chunked' }, ['{"query":"', 'x'.repeat(451) + '"}']), 413, 'BODY_LIMIT');
  const slow = error(post(base, '/v1/responses', { input: query + ' 待機' }, auth), 408, 'REQUEST_TIMEOUT');
  const deadline = Date.now() + 1800;
  while (!model.calls.length && Date.now() < deadline) await delay(10);
  assert.equal(model.calls.length, 1);
  await error(post(base, '/v1/search', { query }, auth), 429, 'REQUEST_LIMIT');
  await slow;
  assert.equal((await ok(post(base, '/v1/search', { query }, auth))).build_id, kb.build.build_id);
  assert.equal(parse(await readFile(kb.configFile, 'utf8')).serve.port, 4318);
});
