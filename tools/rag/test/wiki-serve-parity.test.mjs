// Spec 28.12 step 5b: the screen of Wiki and PARA projects runs in Rust and answers as the
// JavaScript screen (test/fixtures/wiki-reference) did: the same status codes, headers and shown
// content, the same checks of hosts, origins, credentials, forms and uploads, and the same recorded
// work. Two copies of one project are served, one by the CLI and one by the reference. The layout
// around the content (a file tree on the left, the content on the right) is the Rust screen's own,
// so full screens are compared by the content and the work they show (wiki-screen.test.mjs covers
// the layout).
import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import { once } from 'node:events';
import { createServer, request as httpRequest } from 'node:http';
import { cp, mkdtemp, readdir, readFile, realpath, rm, writeFile } from 'node:fs/promises';
import { DatabaseSync } from 'node:sqlite';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { promisify } from 'node:util';
import { parse, stringify } from 'yaml';

import { KnowledgeNest } from 'knowledge-nest';
import { chunkCorpus } from './fixtures/cli-reference/project-content.mjs';
import { atomicWrite } from './fixtures/wiki-reference/files.mjs';
import { readPublication } from './fixtures/wiki-reference/html.mjs';
import { ensureWiki, runWiki, serveWiki, validateWiki } from './fixtures/wiki-reference/project-wiki.mjs';

const cli = fileURLToPath(new URL('../cli.mjs', import.meta.url));
const runNode = promisify(execFile);

async function model(t) {
  const server = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const body = JSON.parse(Buffer.concat(chunks).toString('utf8'));
    const input = JSON.parse(body.messages[1].content);
    const properties = body.format.properties;
    let result;
    if (properties.issues) result = { issues: [] };
    else if (properties.concepts) result = { concepts: [], questions: [], sources: [] };
    else if (properties.pages) {
      result = { pages: input.sources.map(({ id, text }) => {
        const quote = text.split('\n').map((line) => line.trim()).find(Boolean);
        return { id: text.includes('ポンプ') ? 'pump' : 'note-' + id, title: quote.slice(0, 8), facts: [{ text: quote + 'について。<b>x</b>', citations: [{ id, quote }] }] };
      }) };
    } else {
      const [page] = input.pages;
      result = { facts: [{ text: '回答です。', citations: [{ id: 1, quote: page.markdown.split('\n\n')[0].replace(/(?: \[\d+\])+$/u, '').slice(0, 6) }] }] };
    }
    response.setHeader('content-type', 'application/json');
    response.end(JSON.stringify({ done: true, message: { content: JSON.stringify(result) } }));
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  t.after(() => new Promise((resolve) => { server.closeAllConnections(); server.close(resolve); }));
  return 'http://127.0.0.1:' + server.address().port;
}

/** Sends a raw request, with the Host and Origin headers it is given. */
function send(port, { method = 'GET', path = '/', headers = {}, body }) {
  return new Promise((resolve, reject) => {
    const request = httpRequest({ host: '127.0.0.1', port, method, path, headers: { host: '127.0.0.1:' + port, ...headers } }, (response) => {
      const chunks = [];
      response.on('data', (chunk) => chunks.push(chunk));
      response.on('end', () => resolve({ status: response.statusCode, headers: response.headers, body: Buffer.concat(chunks).toString('utf8') }));
    });
    request.on('error', reject);
    request.end(body);
  });
}

/** Asserts that two texts are equal, showing where they first differ. */
function equalText(left, right, label) {
  let at = 0;
  while (at < left.length && left[at] === right[at]) at++;
  assert.equal(left.slice(Math.max(0, at - 300), at + 300), right.slice(Math.max(0, at - 300), at + 300), label);
  assert.equal(left.length, right.length, label);
}

/**
 * What both screens show of a full screen: the content and the work, without the layout and the
 * forms around them. The top page is the Rust screen's own (a home with the state of the project),
 * so only its work is compared.
 */
function screenParts(body) {
  if (!body.startsWith('<!doctype html>')) return body;
  const own = /<article id="content" class="([^"]*)">([\s\S]*?)<\/article>/u.exec(body);
  let center = own ? own[2] : /<\/nav><section>([\s\S]*?)<\/section><aside>/u.exec(body)?.[1] ?? '';
  if (own ? own[1] === 'top' : /^<h1>[^<]*<\/h1><h2>目的と範囲<\/h2>/u.test(center)) center = '';
  const work = /<section aria-live="polite"[\s\S]*?<\/section>/u.exec(body)?.[0] ?? '';
  // On the Rust screen citation numbers jump to their citation, and citations link to their
  // source and open their quoted passage.
  center = center.replace(/<form class="inline"[\s\S]*?<\/form>/gu, '').replace(/<a href="\/\?source=[^"]*">(.*?)<\/a>/gu, '$1')
    .replace(/<a class="ref" href="#cite-\d+">(\d+)<\/a>/gu, ' [$1]').replace(/<li id="cite-\d+">/gu, '<li>');
  // A page of the Rust screen has its contents, and its text and its citations in boxes of their own.
  center = center.replace(/<nav class="toc"[\s\S]*?<\/nav>/gu, '').replace(/<h2 id="section-\d+">/gu, '<h2>')
    .replace('<div class="body">', '').replace('</div><section class="notes">', '').replace(/<\/ol><\/section>$/u, '</ol>');
  return 'content: ' + center + '\nwork: ' + work;
}

/** Replaces form keys, nonces, work IDs, times, digests, ports and project paths by placeholders. */
function normalizer(roots, port) {
  const ids = new Map();
  const placeholder = (map, prefix) => (value) => {
    if (!map.has(value)) map.set(value, prefix + map.size);
    return map.get(value);
  };
  const digests = new Map();
  const sources = new Map();
  return (text) => roots.reduce((value, root) => value.replaceAll(pathToFileURL(root).href, 'file:///ROOT').replaceAll(root, 'ROOT')
    .replaceAll(root.replaceAll('\\', '\\\\'), 'ROOT'), text)
    .replaceAll(':' + port, ':PORT')
    // The sources are listed by ID, and a project file's ID depends on where the copy is.
    .replace(/(<h2>原本<\/h2><ul>)(.*?)(<\/ul>)/u, (_, open, items, close) => open + (items.match(/<li>.*?<\/li>/gu) ?? []).sort().join('') + close)
    .replace(/name="token" value="[0-9a-f-]{36}"/gu, 'name="token" value="KEY"')
    .replace(/nonce-[0-9a-f-]{36}|nonce="[0-9a-f-]{36}"/gu, 'NONCE')
    .replace(/run-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/gu, placeholder(ids, 'run-'))
    .replace(/source-[0-9a-f]{24}/gu, placeholder(sources, 'source-'))
    .replace(/\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z/gu, 'TIME')
    .replace(/(&quot;|")(acquiredAtMs|modifiedAtMs|pid)\1: ?\d+/gu, '$1$2$1:0')
    .replace(/(?<![0-9a-f])[0-9a-f]{64}(?![0-9a-f])/gu, placeholder(digests, 'digest-'));
}

test('serves the Wiki screen like the former JavaScript screen', { timeout: 300_000 }, async (t) => {
  const root = await realpath(await mkdtemp(join(tmpdir(), 'knowledge-nest-wiki-serve-')));
  const url = await model(t);
  const env = { ...process.env, KNOWLEDGE_NEST_GLOBAL_CONFIG: join(root, 'global.yaml'), KNOWLEDGE_NEST_TEST_SCREEN_TOKEN: 'screen-token' };
  await runNode(process.execPath, [cli, 'init', 'wiki', 'kb', '--purpose', '設備保全の手順'], { cwd: root, env });
  const base = join(root, 'kb');
  const configPath = join(base, 'KnowledgeNestfile.yaml');
  const config = parse(await readFile(configPath, 'utf8'));
  config.models.writer.model = 'testing';
  config.models.writer.base_url = url;
  config.serve.port = 0;
  await writeFile(configPath, stringify(config));
  await writeFile(join(base, 'manual.md'), '# ポンプの保守\n\n交換手順を確認する。\n');
  await writeFile(join(base, 'note.txt'), '記録を保管する。\n');
  const cliJson = (...args) => runNode(process.execPath, [cli, '--project', 'kb', ...args, '--json'], { cwd: root, env, encoding: 'utf8' });
  await cliJson('ingress', join(base, 'manual.md'));
  await cliJson('ingress', join(base, 'note.txt'), '--source-only');
  await cliJson('verify', '--content');
  const refBase = join(root, 'ref');
  await cp(base, refBase, { recursive: true });
  // The pages of the CLI are the files of its folder; the reference reads them from its table.
  {
    const db = new DatabaseSync(join(refBase, '.knowledge-nest', 'state.sqlite'));
    ensureWiki(db);
    for (const name of await readdir(join(refBase, 'wiki'), { recursive: true })) {
      if (!name.endsWith('.html')) continue;
      const html = await readFile(join(refBase, 'wiki', name), 'utf8');
      if (!html.includes('id="knowledge-nest-page"')) continue;
      const page = readPublication(html);
      db.prepare('INSERT INTO wiki_pages VALUES (?, ?)').run(page.id, JSON.stringify(page));
    }
    db.close();
  }
  for (const directory of [base, refBase]) await writeFile(join(directory, 'other.txt'), '点検の予定を立てる。\n');

  // The reference screen, in this process.
  const servers = [];
  const startReference = async (settings) => {
    const projectConfig = parse(await readFile(join(refBase, 'KnowledgeNestfile.yaml'), 'utf8'));
    const { plan, serveSettings } = validateWiki(projectConfig, join(refBase, 'KnowledgeNestfile.yaml'), () => null);
    const paths = { data: join(refBase, '.knowledge-nest'), state: join(refBase, '.knowledge-nest', 'state.sqlite'),
      artifacts: join(refBase, '.knowledge-nest', 'artifacts'), corpus: join(refBase, '.knowledge-nest', 'corpus') };
    const project = { root: refBase, configRoot: refBase, config: projectConfig, paths, wikiPlan: plan, serveSettings };
    const db = new DatabaseSync(paths.state);
    db.exec('PRAGMA busy_timeout = 500');
    ensureWiki(db);
    const execute = (action, input, options = {}, id, progress) => runWiki(db, project, action, input, options, id, paths, {
      atomicWrite, warnings: [], progress,
      open: (path, keyword) => KnowledgeNest.open(path, { corpusBackend: 'sqlite', keywordNgrams: true, keywordBigrams: keyword.short_query === 'bigram' }),
      chunk: (document) => chunkCorpus({ groups: [['docs', [document]]], unicode: 'nfkc' }, { size: 800, overlap: 100 })[0][0],
    });
    const server = await serveWiki(db, project, execute, { ...serveSettings, ...settings });
    servers.push(async () => { server.close(); await server.drainWork(); db.close(); });
    return server.address().port;
  };
  // The screen of the CLI, in Rust.
  const startActual = async () => {
    const child = spawn(process.execPath, [cli, '--project', 'kb', 'serve'], { cwd: root, env, stdio: ['ignore', 'ignore', 'pipe'] });
    const stopped = once(child, 'exit');
    servers.push(async () => { child.kill(); await stopped; });
    let log = '';
    for await (const chunk of child.stderr) {
      log += chunk;
      const port = /http:\/\/127\.0\.0\.1:(\d+)/u.exec(log)?.[1];
      if (port) return Number(port);
    }
    throw new Error(log);
  };
  const stopAll = async () => { for (const stop of servers.splice(0).reverse()) await stop(); };
  t.after(async () => { await stopAll(); await rm(root, { recursive: true, force: true, maxRetries: 5 }); });

  let [actualPort, expectedPort] = [await startActual(), await startReference({ port: 0 })];
  let [actualText, expectedText] = [normalizer([base, refBase], actualPort), normalizer([refBase, base], expectedPort)];
  const headers = ['content-type', 'cache-control', 'x-content-type-options', 'content-security-policy', 'location',
    'access-control-allow-origin', 'access-control-allow-credentials', 'access-control-allow-methods', 'vary', 'www-authenticate', 'allow', 'connection'];
  const view = (response, text) => JSON.stringify({ status: response.status,
    headers: Object.fromEntries(headers.filter((name) => response.headers[name] !== undefined && !(name === 'connection' && response.headers[name] === 'keep-alive')).map((name) => [name, text(String(response.headers[name]))])),
    body: text(screenParts(response.body)) });
  /** Sends the same request to both screens and compares the replies. */
  const same = async (label, request) => {
    const own = (port) => typeof request === 'function' ? request(port) : request;
    const actual = await send(actualPort, own(actualPort));
    const expected = await send(expectedPort, own(expectedPort));
    equalText(view(actual, actualText), view(expected, expectedText), label);
    return { actual, expected };
  };
  const origin = (port) => ({ origin: 'http://127.0.0.1:' + port });
  const form = (port, fields) => ({ method: 'POST', path: '/action', body: new URLSearchParams(fields).toString(),
    headers: { ...origin(port), 'content-type': 'application/x-www-form-urlencoded' } });
  const token = async (port) => /name="token" value="([^"]+)"/u.exec((await send(port, {})).body)[1];
  const keys = { [actualPort]: await token(actualPort), [expectedPort]: await token(expectedPort) };
  /** Posts a form to both screens, then waits for both works and compares their pages. */
  const work = async (label, fields, extra = {}) => {
    const { actual, expected } = await same(label, (port) => ({ ...form(port, { token: keys[port], ...fields }), ...extra }));
    if (actual.status !== 303) return;
    const settle = async (port, location) => {
      for (let attempt = 0; attempt < 100; attempt++) {
        const page = await send(port, { path: location });
        if (/data-work-status="(?:succeeded|failed)"/u.test(page.body)) return page;
        await delay(50);
      }
      throw new Error('The work did not finish: ' + location);
    };
    const [left, right] = [await settle(actualPort, actual.headers.location), await settle(expectedPort, expected.headers.location)];
    equalText(view(left, actualText), view(right, expectedText), label + ' result');
  };

  await same('top', {});
  await same('page', { path: '/?page=pump' });
  await same('missing page', { path: '/?page=missing' });
  await same('rules', { path: '/?rules' });
  await same('unknown work', { path: '/?work=run-missing' });
  await same('host', { headers: { host: 'example.invalid' } });
  await same('origin', { headers: { origin: 'http://example.invalid' } });
  await same('own origin', (port) => ({ headers: origin(port) }));
  await same('preflight', (port) => ({ method: 'OPTIONS', headers: { ...origin(port), 'access-control-request-method': 'POST' } }));
  await same('method', { method: 'PUT' });
  await same('path', { path: '//x' });
  await same('body limit', (port) => ({ ...form(port, {}), headers: { ...origin(port), 'content-length': '2000000' }, body: undefined }));
  await same('no origin', (port) => ({ ...form(port, { token: keys[port], action: 'verify' }), headers: { 'content-type': 'application/x-www-form-urlencoded' } }));
  await same('wrong path', (port) => ({ ...form(port, { token: keys[port], action: 'verify' }), path: '/other' }));
  await same('wrong token', (port) => form(port, { token: 'x', action: 'verify' }));
  await same('unknown action', (port) => form(port, { token: keys[port], action: 'bogus', value: 'x' }));
  await same('empty value', (port) => form(port, { token: keys[port], action: 'query', value: ' ' }));
  await same('missing input', (port) => form(port, { token: keys[port], action: 'cite', value: '1' }));
  await same('outside file', (port) => form(port, { token: keys[port], action: 'ingress', value: '../outside.txt' }));
  await same('hidden file', (port) => form(port, { token: keys[port], action: 'ingress', value: '.knowledge-nest/state.sqlite' }));
  await same('folder', (port) => form(port, { token: keys[port], action: 'ingress', value: 'site' }));
  const upload = (name, content) => async (port) => {
    const data = new FormData();
    data.set('token', keys[port]);
    data.set('action', 'ingress');
    data.set('upload', new Blob([content]), name);
    const request = new Request('http://x/', { method: 'POST', body: data });
    return { method: 'POST', path: '/action', headers: { ...origin(port), 'content-type': request.headers.get('content-type') },
      body: Buffer.from(await request.arrayBuffer()) };
  };
  for (const name of ['.hidden.txt', 'con.txt', 'a:b.txt', 'trailing.']) {
    const requests = { [actualPort]: await upload(name, 'x\n')(actualPort), [expectedPort]: await upload(name, 'x\n')(expectedPort) };
    await same('upload ' + name, (port) => requests[port]);
  }
  await work('query', { action: 'query', value: 'ポンプの交換' });
  await work('query on a page', { action: 'query', value: 'ポンプ', page: 'pump', second: '手順' });
  await work('verify', { action: 'verify' });
  await work('content', { action: 'content', page: 'pump' });
  await work('cite', { action: 'cite', page: 'pump', value: '1' });
  await work('ingress a project file', { action: 'ingress', value: 'other.txt' });
  await work('pending', { action: 'pending' });
  await work('archive', { action: 'archive', page: 'pump' });
  await work('restore', { action: 'restore', value: 'pump' });
  await work('rule', { action: 'rule', value: '番号付きで書く' });
  await work('issue', { action: 'issue', value: 'issue-bad', second: 'dismissed' });
  await same('top after work', {});
  await same('page after work', { path: '/?page=pump' });
  await stopAll();

  // With a token: Basic credentials for the browser, or a bearer token.
  for (const path of [configPath, join(refBase, 'KnowledgeNestfile.yaml')]) {
    const next = parse(await readFile(path, 'utf8'));
    next.serve.access.auth = { type: 'token', token_env: 'KNOWLEDGE_NEST_TEST_SCREEN_TOKEN' };
    await writeFile(path, stringify(next));
  }
  process.env.KNOWLEDGE_NEST_TEST_SCREEN_TOKEN = 'screen-token';
  t.after(() => { delete process.env.KNOWLEDGE_NEST_TEST_SCREEN_TOKEN; });
  [actualPort, expectedPort] = [await startActual(), await startReference({ port: 0, access: { ...config.serve.access, auth: { type: 'token', token_env: 'KNOWLEDGE_NEST_TEST_SCREEN_TOKEN' } } })];
  [actualText, expectedText] = [normalizer([base, refBase], actualPort), normalizer([refBase, base], expectedPort)];
  const basic = 'Basic ' + Buffer.from('knowledge-nest:screen-token').toString('base64');
  await same('no credentials', {});
  await same('wrong credentials', { headers: { authorization: 'Basic ' + Buffer.from('knowledge-nest:wrong').toString('base64') } });
  await same('other user', { headers: { authorization: 'Basic ' + Buffer.from('admin:screen-token').toString('base64') } });
  await same('basic', { headers: { authorization: basic } });
  await same('bearer', { headers: { authorization: 'Bearer screen-token' } });
  await same('credentials with origin', (port) => ({ headers: { authorization: basic, ...origin(port) } }));
  await same('preflight with credentials', (port) => ({ method: 'OPTIONS', headers: { ...origin(port), 'access-control-request-method': 'POST' } }));
});
