// The screen of Wiki and PARA projects: a file tree of pages and sources on the left, the content
// on the right, and sources added from the browser by web address. Web addresses also work with
// `ingress` on the command line.
import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import { once } from 'node:events';
import { createServer, request as httpRequest } from 'node:http';
import { mkdtemp, readFile, realpath, rm, writeFile } from 'node:fs/promises';
import { networkInterfaces, tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { parse, stringify } from 'yaml';

const cli = fileURLToPath(new URL('../cli.mjs', import.meta.url));
const runNode = promisify(execFile);

/** Starts a server for the test and stops it afterwards. */
async function listen(stops, handler) {
  const server = createServer(handler);
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  stops.push(() => new Promise((resolve) => { server.closeAllConnections(); server.close(resolve); }));
  return 'http://127.0.0.1:' + server.address().port;
}

/** A writing model that turns the first line of each source into a page. */
function model(stops) {
  return listen(stops, async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const body = JSON.parse(Buffer.concat(chunks).toString('utf8'));
    const input = JSON.parse(body.messages[1].content);
    let result = { issues: [] };
    if (body.format.properties.pages) {
      result = { pages: input.sources.map(({ id, text }) => {
        const quote = text.split('\n').map((line) => line.trim()).find(Boolean);
        return { id: 'guides/note-' + id, title: quote.slice(0, 8), facts: [{ text: quote, citations: [{ id, quote }] }] };
      }) };
    }
    response.setHeader('content-type', 'application/json');
    response.end(JSON.stringify({ done: true, message: { content: JSON.stringify(result) } }));
  });
}

/** A web site: a page in Shift_JIS without an extension, and a Markdown file. */
function site(stops) {
  return listen(stops, (request, response) => {
    if (request.url === '/manuals/pump') {
      const html = '<html><head><meta charset="shift_jis"><title>ポンプ</title></head><body><h1>ポンプの点検</h1><p>毎月第一営業日に点検する。</p></body></html>';
      response.setHeader('content-type', 'text/html; charset=Shift_JIS');
      response.end(shiftJis(html));
    } else if (request.url === '/notes.md') {
      response.setHeader('content-type', 'text/markdown; charset=utf-8');
      response.end('# 記録\n\n点検の記録を保管する。\n');
    } else {
      response.statusCode = 404;
      response.end('missing');
    }
  });
}

/** Shift_JIS bytes of a text, from the decoder's table of every double-byte character. */
function shiftJis(text) {
  const decoder = new TextDecoder('shift_jis');
  const table = new Map();
  for (let lead = 0x81; lead <= 0xfc; lead++) {
    if (lead > 0x9f && lead < 0xe0) continue;
    for (let trail = 0x40; trail <= 0xfc; trail++) {
      const character = decoder.decode(Uint8Array.of(lead, trail));
      if (!table.has(character)) table.set(character, [lead, trail]);
    }
  }
  return Buffer.from([...text].flatMap((character) => character.charCodeAt(0) < 0x80 ? [character.charCodeAt(0)] : table.get(character)));
}

/** Sends a request to the screen, as the browser on its own origin does. */
function send(port, { method = 'GET', path = '/', headers = {}, body, address = '127.0.0.1' }) {
  return new Promise((resolve, reject) => {
    const request = httpRequest({ host: address, port, method, agent: false, path, headers: { host: address + ':' + port, ...headers } }, (response) => {
      const chunks = [];
      response.on('data', (chunk) => chunks.push(chunk));
      response.on('end', () => resolve({ status: response.statusCode, headers: response.headers, body: Buffer.concat(chunks).toString('utf8') }));
    });
    request.on('error', reject);
    request.end(body);
  });
}

test('shows pages and sources as a tree and adds sources by web address', { timeout: 300_000 }, async (t) => {
  const root = await realpath(await mkdtemp(join(tmpdir(), 'knowledge-nest-wiki-screen-')));
  // Hooks run in the order they were added: the screen stops before its files are removed.
  const stops = [];
  t.after(async () => {
    for (const stop of stops.reverse()) await stop();
    await rm(root, { recursive: true, force: true, maxRetries: 5 });
  });
  const [writer, web] = [await model(stops), await site(stops)];
  const env = { ...process.env, KNOWLEDGE_NEST_GLOBAL_CONFIG: join(root, 'global.yaml') };
  await runNode(process.execPath, [cli, 'init', 'wiki', 'kb', '--purpose', '設備保全の手順'], { cwd: root, env });
  const base = join(root, 'kb');
  const configPath = join(base, 'KnowledgeNestfile.yaml');
  const config = parse(await readFile(configPath, 'utf8'));
  config.models.writer.model = 'testing';
  config.models.writer.base_url = writer;
  config.serve.port = 0;
  await writeFile(configPath, stringify(config));
  const nest = async (...args) => JSON.parse((await runNode(process.execPath, [cli, '--project', 'kb', ...args, '--json'], { cwd: root, env, encoding: 'utf8' })).stdout);

  // The command line: a web address is a source, read before anything is written.
  const planned = await nest('ingress', web + '/notes.md', '--dry-run');
  assert.equal(planned.data.files, 1);
  await nest('ingress', web + '/notes.md', '--source-only');
  await assert.rejects(nest('ingress', web + '/missing'), (error) => /"FETCH_FAILED".*HTTP 404/u.test(error.stdout));

  const child = spawn(process.execPath, [cli, '--project', 'kb', 'serve'], { cwd: root, env, stdio: ['ignore', 'ignore', 'pipe'] });
  const stopped = once(child, 'exit');
  stops.push(async () => { child.kill(); await stopped; });
  let log = '';
  let port;
  for await (const chunk of child.stderr) {
    log += chunk;
    port = /http:\/\/127\.0\.0\.1:(\d+)/u.exec(log)?.[1];
    if (port) break;
  }
  assert.ok(port, log);
  const origin = { origin: 'http://127.0.0.1:' + port };

  // The left column: the source from the command line, under its host, not yet in the pages.
  const top = await send(port, {});
  assert.equal(top.status, 200);
  const host = new URL(web).host;
  assert.match(top.body, /<nav>.*<summary>資料を取り込む<\/summary>.*<input type="url" name="url"/su);
  assert.match(top.body, new RegExp(`<h2>原本<small>取り込んだ資料</small></h2><ul class="tree"><li><details open><summary>Web</summary><ul class="tree"><li><details open><summary>${host.replace(/\./gu, '\\.')}</summary>`, 'u'));
  assert.match(top.body, /<a class="web" href="\/\?source=source-[0-9a-f]{24}" title="http:\/\/127\.0\.0\.1:\d+\/notes\.md"><span class="name">\/notes\.md<\/span><span class="badge">未反映<\/span><\/a>/u);
  // The home: what the project is for, a question box, and its state with the next thing to do.
  assert.match(top.body, /<article id="content" class="top"><h1>kb<\/h1><p class="lead">設備保全の手順<\/p><form class="ask"/u);
  assert.match(top.body, /<a class="tile" href="#pages"><b>0<\/b><span>ページ<\/span>/u);
  assert.match(top.body, /<div class="tile warn"><b>1<\/b><span>未反映<\/span>.*?<button name="action" value="pending">反映する<\/button>/u);
  assert.match(top.body, /<h2>最近の作業<\/h2><ul class="works"><li><a href="\/\?work=run-[0-9a-f-]+"><span class="dot failed"><\/span><strong>取り込み<\/strong>/u);
  const history = await send(port, { path: '/?history' });
  assert.match(history.body, /<a href="\/\?history" aria-current="page">作業の履歴<\/a>/u);
  assert.match(history.body, /<article id="content" class="history"><h1>作業の履歴<\/h1>/u);

  // The right column: a source shows where it came from and its text.
  const sourceLink = /href="\/\?(source=source-[0-9a-f]{24})"/u.exec(top.body)[1];
  const source = await send(port, { path: '/?' + sourceLink });
  assert.match(source.body, /<div class="crumbs"><span>原本<\/span><span>Web<\/span><span>127\.0\.0\.1:\d+<\/span><span>\/notes\.md<\/span><\/div><article id="content" class="source"><h1>http:\/\/127\.0\.0\.1:\d+\/notes\.md<\/h1>/u);
  assert.match(source.body, /<a href="http:\/\/127\.0\.0\.1:\d+\/notes\.md" rel="noreferrer noopener" target="_blank">/u);
  assert.match(source.body, /点検の記録を保管する。/u);
  // What can be done with it: write it into the pages, or fetch its address again.
  assert.match(source.body, /<button class="primary" name="action" value="pending">未反映の原本を反映する<\/button>/u);
  assert.match(source.body, /<input type="hidden" name="url" value="http:\/\/127\.0\.0\.1:\d+\/notes\.md"><button name="action" value="ingress">この URL を取り直す<\/button>/u);
  assert.match(source.body, new RegExp(`href="/\\?${sourceLink}" title="[^"]*" aria-current="page"`, 'u'));

  // Adding a page by its address from the browser.
  const token = /name="token" value="([^"]+)"/u.exec(top.body)[1];
  const post = (fields) => send(port, { method: 'POST', path: '/action', body: new URLSearchParams({ token, ...fields }).toString(),
    headers: { ...origin, 'content-type': 'application/x-www-form-urlencoded' } });
  assert.equal((await post({ action: 'ingress', url: 'ftp://example.invalid/a' })).status, 400);
  const added = await post({ action: 'ingress', url: web + '/manuals/pump#top' });
  assert.equal(added.status, 303);
  let done;
  for (let attempt = 0; attempt < 200 && !done; attempt++) {
    const page = await send(port, { path: added.headers.location });
    if (/data-work-status="(?:succeeded|failed)"/u.test(page.body)) done = page;
    else await delay(50);
  }
  assert.match(done.body, /data-work-status="succeeded"/u, done.body);
  assert.match(done.body, /<div class="work succeeded"><header><span class="dot succeeded"><\/span><strong>取り込み<\/strong><span>完了<\/span><\/header><p>原本 1 件を読みました。<\/p><p>書いたページ：<a href="\/\?page=guides%2Fnote-1">/u);

  // The new page sits in its folder; the page cites the web page, decoded from Shift_JIS.
  const after = await send(port, {});
  assert.match(after.body, /<h2>ページ<small>AI がまとめた記事<\/small><\/h2><ul class="tree"><li><details open><summary>guides<\/summary>/u);
  assert.match(after.body, /title="http:\/\/127\.0\.0\.1:\d+\/manuals\/pump"><span class="name">\/manuals\/pump<\/span><\/a><\/li>/u);
  const pageId = /href="\/\?page=([^"]+)"/u.exec(after.body)[1];
  const page = await send(port, { path: '/?page=' + pageId });
  assert.match(page.body, /<h2>出典<\/h2><ol><li id="cite-1"><a href="\/\?source=source-[0-9a-f]{24}">http:\/\/127\.0\.0\.1:\d+\/manuals\/pump<\/a>/u);
  // Citation numbers in the text jump to their citation.
  assert.match(page.body, /<a class="ref" href="#cite-1">1<\/a>/u);
  assert.match(page.body, /<div class="crumbs"><span>ページ<\/span><span>guides<\/span><span>[^<]*<\/span><em>更新 [^<]* · 出典 1 件<\/em><\/div>/u);
  // The actions of a page sit under it: ask about it, have it revised, and manage it.
  assert.match(page.body, /<h2>このページでできること<\/h2><details name="act" open><summary>質問する<\/summary>.*<summary>AI に指示して直す<\/summary>.*<summary>検査・アーカイブ<\/summary>/su);
  const cited = await post({ action: 'cite', page: decodeURIComponent(pageId), value: '1' });
  assert.equal(cited.status, 303);
  let quoted;
  for (let attempt = 0; attempt < 200 && !quoted; attempt++) {
    const shown = await send(port, { path: cited.headers.location });
    if (/data-work-status="(?:succeeded|failed)"/u.test(shown.body)) quoted = shown;
    else await delay(50);
  }
  assert.match(quoted.body, /<strong>引用の確認<\/strong><span>完了<\/span><\/header><blockquote>[^<]*ポンプの点検[^<]*<\/blockquote>/u);
  const pumpLink = /<a href="\/\?(source=source-[0-9a-f]{24})">http:\/\/127\.0\.0\.1:\d+\/manuals\/pump<\/a>/u.exec(page.body)[1];
  const pump = await send(port, { path: '/?' + pumpLink });
  assert.match(pump.body, /毎月第一営業日に点検する。/u);
  assert.match(pump.body, /<h2>引用しているページ<\/h2><ul><li><a href="\/\?page=/u);
});

test('lets in the machine itself and the clients of the listed ranges', { timeout: 120_000 }, async (t) => {
  const root = await realpath(await mkdtemp(join(tmpdir(), 'knowledge-nest-wiki-clients-')));
  const stops = [];
  t.after(async () => {
    for (const stop of stops.reverse()) await stop();
    await rm(root, { recursive: true, force: true, maxRetries: 5 });
  });
  const env = { ...process.env, KNOWLEDGE_NEST_GLOBAL_CONFIG: join(root, 'global.yaml'), KNOWLEDGE_NEST_TEST_TOKEN: 'local-test-token' };
  await runNode(process.execPath, [cli, 'init', 'wiki', 'kb', '--purpose', '設備保全の手順'], { cwd: root, env });
  const configPath = join(root, 'kb', 'KnowledgeNestfile.yaml');
  const config = parse(await readFile(configPath, 'utf8'));
  // An address of this machine other than loopback: a request sent to it also comes from it.
  const own = Object.values(networkInterfaces()).flat().find((entry) => entry.family === 'IPv4' && !entry.internal)?.address;
  Object.assign(config.serve, { host: '0.0.0.0', port: 0 });
  config.serve.access.allowed_hosts.push(...(own ? [own] : []));
  config.serve.access.auth = { type: 'token', token_env: 'KNOWLEDGE_NEST_TEST_TOKEN' };
  const save = async (clients) => {
    config.serve.access.allowed_clients = clients;
    await writeFile(configPath, stringify(config));
  };
  const validate = () => runNode(process.execPath, [cli, '--project', 'kb', 'validate', '--json'], { cwd: root, env, encoding: 'utf8' });

  // A range is an address with the number of leading bits; a list that is given names someone.
  for (const clients of [['192.168.11.*'], ['192.168.11.0/33'], []]) {
    await save(clients);
    await assert.rejects(validate(), (error) => {
      const [found] = JSON.parse(error.stdout).errors;
      return error.code === 2 && found.code === 'ARG_RANGE' && found.section === 'serve';
    }, JSON.stringify(clients));
  }

  const start = async (clients) => {
    await save(clients);
    const child = spawn(process.execPath, [cli, '--project', 'kb', 'serve'], { cwd: root, env, stdio: ['ignore', 'ignore', 'pipe'] });
    const stopped = once(child, 'exit');
    const stop = async () => { child.kill(); await stopped; };
    stops.push(stop);
    let log = '';
    for await (const chunk of child.stderr) {
      log += chunk;
      const port = /http:\/\/0\.0\.0\.0:(\d+)/u.exec(log)?.[1];
      if (port) return { port, stop };
    }
    assert.fail(log);
  };
  const headers = { authorization: 'Bearer local-test-token' };

  // Nobody of the listed range is here: the machine itself still gets in, its other address does not.
  const closed = await start(['192.0.2.0/24']);
  assert.equal((await send(closed.port, { headers })).status, 200);
  assert.equal((await send(closed.port, {})).status, 401);
  if (own) {
    const refused = await send(closed.port, { headers, address: own });
    assert.equal(refused.status, 403);
    assert.match(refused.body, /CLIENT_FORBIDDEN/u);
  }
  await closed.stop();

  if (!own) return t.diagnostic('no address other than loopback: the listed range was not tried');
  const open = await start(['192.0.2.0/24', own + '/24']);
  assert.equal((await send(open.port, { headers, address: own })).status, 200);
  assert.equal((await send(open.port, { address: own })).status, 401);
});
