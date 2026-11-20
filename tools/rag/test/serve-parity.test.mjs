// Spec 28.10 step 3c: the Rust HTTP server of serve answers like the former JavaScript server
// (test/fixtures/rag-http-reference-serve.mjs): status codes, headers and bodies of /v1/search,
// /v1/responses and /mcp, and the checks of hosts, origins, tokens, paths and body sizes.
import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import { once } from 'node:events';
import { createServer, request as httpRequest } from 'node:http';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import { currentSearchSchemaText } from './fixtures/search-schema.mjs';
import { promisify } from 'node:util';
import { parse, stringify } from 'yaml';

const cli = fileURLToPath(new URL('../cli.mjs', import.meta.url));
const reference = fileURLToPath(new URL('./fixtures/rag-http-reference-serve.mjs', import.meta.url));
const runNode = promisify(execFile);
const token = 'parity-token';

/** An answer model that cites the first evidence, or cites outside the context for 不正. */
async function model(t) {
  const server = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const input = JSON.parse(JSON.parse(Buffer.concat(chunks)).messages[1].content);
    const facts = [{ text: input.evidence[0].text.slice(0, 10), evidence_ids: [input.question.includes('不正') ? 99 : 1] }];
    response.setHeader('content-type', 'application/json');
    response.end(JSON.stringify({ done: true, prompt_eval_count: 5, eval_count: 2, message: { content: JSON.stringify({ facts }) } }));
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  t.after(() => new Promise((resolve) => { server.closeAllConnections(); server.close(resolve); }));
  return 'http://127.0.0.1:' + server.address().port;
}

/** Starts a server and returns its base URL from the line it prints. */
async function start(t, args, cwd, env) {
  const child = spawn(process.execPath, args, { cwd, env, stdio: ['ignore', 'ignore', 'pipe'] });
  t.after(async () => {
    child.kill();
    await once(child, 'exit').catch(() => {});
  });
  let text = '';
  child.stderr.setEncoding('utf8');
  return new Promise((resolve, reject) => {
    child.stderr.on('data', (chunk) => {
      text += chunk;
      const match = /Dunlin rag: (http:\/\/[^\s]+)\//u.exec(text);
      if (match) resolve(new URL(match[1]));
    });
    child.once('exit', (code) => reject(new Error('The server stopped (' + code + '): ' + text)));
  });
}

/** A raw request, so that Host and every other header are sent as given. */
function send(base, { method = 'POST', path = '/v1/search', headers = {}, body }) {
  return new Promise((resolve, reject) => {
    const request = httpRequest({ host: base.hostname, port: base.port, method, path,
      headers: { host: base.host, ...headers }, setHost: false }, (response) => {
      const chunks = [];
      response.on('data', (chunk) => chunks.push(chunk));
      response.on('end', () => resolve({ status: response.statusCode, headers: response.headers, body: Buffer.concat(chunks).toString('utf8') }));
    });
    request.on('error', reject);
    request.end(body);
  });
}

const compared = ['content-type', 'cache-control', 'x-content-type-options', 'allow', 'www-authenticate', 'access-control-allow-origin',
  'access-control-expose-headers', 'access-control-allow-methods', 'access-control-allow-headers', 'vary', 'x-accel-buffering'];

/** The parts of a Responses API object that change on every call. */
function stable(body) {
  if (!body.startsWith('{"id":"resp_')) return body;
  const response = JSON.parse(body);
  return JSON.stringify({ ...response, id: 'resp', created_at: 0, output: response.output.map((item) => ({ ...item, id: 'msg' })) });
}

test('serves HTTP like the former JavaScript server', { timeout: 180_000 }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-serve-parity-'));
  const env = { ...process.env, DUNLIN_GLOBAL_CONFIG: join(root, 'global.yaml'), DUNLIN_PARITY_TOKEN: token };
  await runNode(process.execPath, [cli, 'init', 'rag', 'kb', '--no-embedding'], { cwd: root, env });
  await writeFile(join(root, 'kb', 'docs', 'guide.md'), '# 手順\n\nポンプの交換手順です。\n\n## 点検\n\n月に一度ポンプを点検します。\n');
  const configPath = join(root, 'kb', 'Dunlinfile.yaml');
  const config = parse(await readFile(configPath, 'utf8'));
  config.models.answerer = { provider: 'ollama', kind: 'chat', model: 'local', base_url: await model(t) };
  config.serve.access.allowed_origins = ['https://reader.example'];
  config.serve.access.auth = { type: 'token', token_env: 'DUNLIN_PARITY_TOKEN' };
  config.serve.limits = { max_request_bytes: 2000, request_timeout_seconds: 30, max_concurrent: 4 };
  await writeFile(configPath, stringify(config));
  await runNode(process.execPath, [cli, '--project', 'kb', 'build'], { cwd: root, env });
  const [actual, expected] = await Promise.all([
    start(t, [cli, '--project', 'kb', 'serve', '--port', '0'], root, env),
    start(t, [reference, join(root, 'kb')], root, env),
  ]);
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));

  const auth = { authorization: 'Bearer ' + token };
  const json = { ...auth, 'content-type': 'application/json' };
  const mcp = { ...json, accept: 'application/json, text/event-stream' };
  const rpc = (body, headers = mcp) => ({ path: '/mcp', headers, body: JSON.stringify(body) });
  const cases = {
    search: { headers: json, body: JSON.stringify({ query: 'ポンプ' }) },
    searchOptions: { headers: json, body: JSON.stringify({ query: 'ポンプ', limit: 1, budget_chars: 60, source_ids: ['docs'] }) },
    searchUnknownField: { headers: json, body: JSON.stringify({ query: 'ポンプ', extra: 1 }) },
    searchBlank: { headers: json, body: JSON.stringify({ query: '   ' }) },
    searchBadLimit: { headers: json, body: JSON.stringify({ query: 'ポンプ', limit: 101 }) },
    searchBadSource: { headers: json, body: JSON.stringify({ query: 'ポンプ', source_ids: ['bad id'] }) },
    searchInvalidJson: { headers: json, body: '{' },
    searchText: { headers: { ...auth, 'content-type': 'text/plain' }, body: '{}' },
    searchCharset: { headers: { ...auth, 'content-type': 'application/json ; charset=utf-8' }, body: JSON.stringify({ query: 'ポンプ' }) },
    searchGet: { method: 'GET', headers: auth },
    missing: { path: '/v1/missing', headers: json, body: '{}' },
    noToken: { headers: { 'content-type': 'application/json' }, body: '{}' },
    wrongToken: { headers: { authorization: 'Bearer wrong', 'content-type': 'application/json' }, body: '{}' },
    hostForbidden: { headers: { ...json, host: 'unexpected.example' }, body: '{}' },
    originForbidden: { headers: { ...json, origin: 'https://other.example' }, body: '{}' },
    originAllowed: { headers: { ...json, origin: 'https://reader.example' }, body: JSON.stringify({ query: 'ポンプ' }) },
    preflight: { method: 'OPTIONS', headers: { origin: 'https://reader.example', 'access-control-request-method': 'POST' } },
    tooLarge: { headers: { ...json, 'content-length': '3000' }, body: 'x'.repeat(3000) },
    pathSlashes: { path: '//v1/search', headers: json, body: '{}' },
    answer: { path: '/v1/responses', headers: json, body: JSON.stringify({ input: 'ポンプの交換' }) },
    answerMessages: { path: '/v1/responses', headers: json, body: JSON.stringify({ input: [{ role: 'user', content: '前' },
      { role: 'assistant', content: '後' }, { role: 'user', content: [{ type: 'input_text', text: 'ポンプ' }] }], model: 'local', stream: false }) },
    answerUnsupported: { path: '/v1/responses', headers: json, body: JSON.stringify({ input: 'ポンプ', stream: true }) },
    answerModel: { path: '/v1/responses', headers: json, body: JSON.stringify({ input: 'ポンプ', model: 'other' }) },
    answerOutputText: { path: '/v1/responses', headers: json, body: JSON.stringify({ input: [{ role: 'user', content: [{ type: 'output_text', text: 'x' }] }] }) },
    answerNoUser: { path: '/v1/responses', headers: json, body: JSON.stringify({ input: [{ role: 'assistant', content: 'x' }] }) },
    answerCitation: { path: '/v1/responses', headers: json, body: JSON.stringify({ input: 'ポンプ 不正' }) },
    mcpInitialize: rpc({ jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'p', version: '1' } } }),
    mcpInitializeOld: rpc({ jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '1999-01-01', capabilities: {}, clientInfo: { name: 'p', version: '1' } } }),
    mcpNotification: rpc({ jsonrpc: '2.0', method: 'notifications/initialized' }),
    mcpList: rpc({ jsonrpc: '2.0', id: 2, method: 'tools/list' }),
    mcpCall: rpc({ jsonrpc: '2.0', id: 'a', method: 'tools/call', params: { name: 'knowledge.search', arguments: { query: 'ポンプ', limit: 1 } } }),
    mcpCallInvalid: rpc({ jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'knowledge.search', arguments: { query: 'x', limit: 0 } } }),
    mcpCallTwice: rpc({ jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'knowledge.search', arguments: { query: 'x', budgetChars: 5, budget_chars: 5 } } }),
    mcpUnknownTool: rpc({ jsonrpc: '2.0', id: 4, method: 'tools/call', params: { name: 'knowledge.read', arguments: {} } }),
    mcpNoParams: rpc({ jsonrpc: '2.0', id: 5, method: 'tools/call' }),
    mcpUnknownMethod: rpc({ jsonrpc: '2.0', id: 6, method: 'foo/bar' }),
    mcpBatch: { path: '/mcp', headers: mcp, body: JSON.stringify([{ jsonrpc: '2.0', method: 'notifications/initialized' },
      { jsonrpc: '2.0', id: 7, method: 'ping' }, { jsonrpc: '2.0', id: 8, method: 'tools/list' }]) },
    mcpEmptyBatch: { path: '/mcp', headers: mcp, body: '[]' },
    mcpInvalid: rpc({ jsonrpc: '2.0', id: 9 }),
    mcpBadJson: { path: '/mcp', headers: mcp, body: '{' },
    mcpAcceptJson: rpc({ jsonrpc: '2.0', id: 10, method: 'ping' }, { ...json, accept: 'application/json' }),
    mcpText: rpc({ jsonrpc: '2.0', id: 10, method: 'ping' }, { ...mcp, 'content-type': 'text/plain' }),
    mcpVersion: rpc({ jsonrpc: '2.0', id: 11, method: 'tools/list' }, { ...mcp, 'mcp-protocol-version': '1999-01-01' }),
    mcpGet: { method: 'GET', path: '/mcp', headers: { ...auth, accept: 'text/event-stream' } },
    mcpDelete: { method: 'DELETE', path: '/mcp', headers: auth },
    mcpPut: { method: 'PUT', path: '/mcp', headers: auth },
  };
  // Replies that say more than the former server did: the JSON error and the configured model. The
  // search tool's schema is compared after currentSearchSchema.
  const changed = {
    searchInvalidJson: (body) => assert.match(JSON.parse(body).error.message, /^Provide valid JSON: EOF while parsing/u),
    answerModel: (body) => assert.equal(JSON.parse(body).error.message, 'This project answers with local. Leave out model, or set it to local.'),
  };
  for (const [name, request] of Object.entries(cases)) {
    const [a, b] = await Promise.all([send(actual, request), send(expected, request)]);
    assert.equal(a.status, b.status, name);
    for (const header of compared) assert.equal(a.headers[header], b.headers[header], name + ' ' + header);
    if (a.status === 413) assert.equal(a.headers.connection, b.headers.connection, name);
    if (changed[name]) changed[name](a.body);
    else assert.equal(stable(a.body), stable(currentSearchSchemaText(b.body)), name);
  }
  const latin1 = await send(actual, { headers: json, body: Buffer.from('{"query":"é"}', 'latin1') });
  assert.equal(latin1.status, 400);
  assert.match(JSON.parse(latin1.body).error.message, /^Send the request body as UTF-8 JSON\./u);
});
