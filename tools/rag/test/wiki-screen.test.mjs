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
    if (body.format.properties.concepts) result = { concepts: [], questions: ['点検の周期は誰が決めるか'], sources: [] };
    if (body.format.properties.pages) {
      result = { pages: input.sources.map(({ id, text }) => {
        const quote = text.split('\n').map((line) => line.trim()).find(Boolean);
        return { id: 'guides/note-' + id, title: quote.slice(0, 8), facts: [{ text: '## 概要\n\n## 手順\n\n## 根拠\n\n' + quote, citations: [{ id, quote }] }] };
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
  // A page read from a web address is kept with the raw sources, under its host and path.
  assert.equal(await readFile(join(base, 'raw', '127.0.0.1', 'notes.md'), 'utf8'), '# 記録\n\n点検の記録を保管する。\n');
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

  // The left column: raw/ as it is. The page from the web is kept under its host, not yet in
  // the pages.
  const top = await send(port, {});
  assert.equal(top.status, 200);
  const host = new URL(web).host;
  assert.match(top.body, /<nav>.*<summary>資料を取り込む<\/summary>.*<input type="url" name="url"/su);
  assert.match(top.body, /<h2 class="root">raw\/<small>集めた資料（原本）<\/small><\/h2><ul class="tree"><li><details open><summary>127\.0\.0\.1<\/summary><ul class="tree"><li><a class="file" href="\/\?source=source-[0-9a-f]{24}" title="127\.0\.0\.1\/notes\.md"><span class="name">notes\.md<\/span><span class="badge">未反映<\/span><\/a><\/li><\/ul><\/details><\/li><\/ul>/u);
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
  assert.match(await readFile(join(base, 'raw', '127.0.0.1', 'manuals', 'pump.html'), 'utf8'), /ポンプの点検/u);
  assert.match(done.body, /<div class="work succeeded"><header><span class="dot succeeded"><\/span><strong>取り込み<\/strong><span>完了<\/span><\/header><p>原本 1 件を読みました。<\/p><p>書いたページ：<a href="\/\?page=guides%2Fnote-1">/u);

  // The new page sits in its folder; the page cites the web page, decoded from Shift_JIS.
  const after = await send(port, {});
  assert.match(after.body, /<h2 class="root">wiki\/<small>AI が書くページ<\/small><\/h2><ul class="tree"><li><details open><summary>guides<\/summary>/u);
  assert.match(after.body, /title="127\.0\.0\.1\/manuals\/pump\.html"><span class="name">pump\.html<\/span><\/a><\/li>/u);
  // The top is the contents of the Wiki: every page under its folder, with a line about it.
  assert.match(after.body, /<section class="panel" id="pages"><h2>guides\/<small>1<\/small><\/h2><ul class="cards"><li><a href="\/\?page=guides%2Fnote-\d+"><strong>[^<]+<\/strong><span>概要<\/span><time>出典 1 · \d{4}-\d\d-\d\d<\/time><\/a><\/li><\/ul><\/section>/u);
  assert.match(after.body, /<form class="ask" method="get" action="\/"><input type="search" name="q" required placeholder="ページを検索する"/u);
  assert.match(after.body, /<h2>AI に質問する<\/h2>.*<button class="primary" name="action" value="query">質問する<\/button>.*<h2>保守<\/h2>/su);
  // A search finds the pages that hold every word, and marks the words in the passage.
  const found = await send(port, { path: '/?q=' + encodeURIComponent('点検 ポンプ') });
  assert.match(found.body, /<article id="content" class="search"><h1>検索<\/h1>.*<p class="meta found">1 件のページが見つかりました。<\/p><ul class="cards hits"><li><a href="\/\?page=guides%2Fnote-\d+">/su);
  assert.match(found.body, /<mark>ポンプ<\/mark>の<mark>点検<\/mark>/u);
  const none = await send(port, { path: '/?q=' + encodeURIComponent('点検 zebra') });
  assert.match(none.body, /<p class="meta found">該当するページがありません。/u);
  // The folder of pages has its contents and its log as files of their own.
  const contents = await readFile(join(base, 'wiki', 'index.html'), 'utf8');
  assert.match(contents, /<!-- knowledge-nest:catalog -->/u);
  assert.match(contents, /<h2 class="root">wiki\/<small>AI が書くページ · 1 件<\/small><\/h2><section data-group><h3>guides\/<small>1<\/small><\/h3><ul class="pages"><li data-text="[^"]*"><a href="guides\/note-\d+\.html"><strong>/u);
  // raw/ as it is, each file with what became of it.
  assert.match(contents, /<h2 class="root"><a href="\.\.\/raw\/">raw\/<\/a><small>集めた資料（原本）· 2 件<\/small><\/h2><section data-group><h3>127\.0\.0\.1\/<small>1<\/small><\/h3>.*<h3>127\.0\.0\.1\/manuals\/<small>1<\/small><\/h3>/su);
  assert.match(contents, /<a href="\.\.\/raw\/127\.0\.0\.1\/notes\.md"><strong>notes\.md<\/strong><small class="todo">未反映<\/small><\/a>/u);
  assert.match(contents, /<p class="meta">ページ 1 件 · 原本 2 件（未反映 1 件） · 最終更新 \d{4}-\d\d-\d\d<\/p>/u);
  // The log names each work at the start of a line, oldest first: the day, what was done, and the source.
  const record = await readFile(join(base, 'wiki', 'log.html'), 'utf8');
  assert.match(record, /\n<h2>\[\d{4}-\d\d-\d\d\] ingest \| http:\/\/127\.0\.0\.1:\d+\/manuals\/pump<\/h2>\n<p>原本 1 件 · ページ 1 件<\/p>\n<ul><li><a href="guides\/note-\d+\.html">[^<]+<\/a>（新規）<\/li><\/ul>\n<!-- knowledge-nest:log-through \d+ -->\n/u);
  assert.ok(record.indexOf('ingest | 原本 1 件') < record.indexOf('/manuals/pump</h2>'), 'the earlier work comes first');
  const pageId = /href="\/\?page=([^"]+)"/u.exec(after.body)[1];
  // The file of the page links up to the contents, and to its source by a path of its own.
  const pageFile = await readFile(join(base, 'wiki', ...decodeURIComponent(pageId).split('/')) + '.html', 'utf8');
  assert.match(pageFile, /<main><nav class="up"><a href="\.\.\/index\.html">目録<\/a><span>guides<\/span><\/nav><h1>/u);
  assert.match(pageFile, /<nav class="toc" aria-label="目次">.*<article class="body"><h2 id="section-1">概要<\/h2>/su);
  assert.match(pageFile, /<a class="ref" href="#source-1">1<\/a>.*<section class="notes"><h2>出典<\/h2><ol><li id="source-1">/su);
  const page = await send(port, { path: '/?page=' + pageId });
  // A page of three sections or more has its contents, and each entry jumps to its section.
  assert.match(page.body, /<nav class="toc" aria-label="目次"><h2>目次<\/h2><ol><li><a href="#section-1">概要<\/a><\/li><li><a href="#section-2">手順<\/a><\/li><li><a href="#section-3">根拠<\/a><\/li><\/ol><\/nav><div class="body"><h2 id="section-1">概要<\/h2>/u);
  assert.match(page.body, /<h2 id="section-3">根拠<\/h2>.*<\/div><section class="notes"><h2>出典<\/h2><ol><li id="source-1">/su);
  assert.match(page.body, /<h2>出典<\/h2><ol><li id="source-1"><a href="\/\?source=source-[0-9a-f]{24}">http:\/\/127\.0\.0\.1:\d+\/manuals\/pump<\/a> <span class="lines"><form class="inline"[^>]*>.*?<button name="action" value="cite" title="引用箇所を見る">L\d+–\d+<\/button><\/form><\/span><\/li>/u);
  // Citation numbers in the text jump to their citation.
  assert.match(page.body, /<a class="ref" href="#source-1">1<\/a>/u);
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
  await open.stop();

  // Without a token, the screen opens outside loopback only to home and office networks.
  config.serve.access.auth = { type: 'none', token_env: null };
  const conflict = (error) => error.code === 2 && JSON.parse(error.stdout).errors[0].code === 'ARG_CONFLICT';
  for (const clients of [undefined, ['0.0.0.0/0'], ['192.168.11.0/24', '203.0.113.0/24'], ['192.168.0.0/15']]) {
    await save(clients);
    await assert.rejects(validate(), conflict, JSON.stringify(clients));
  }
  await save(['192.168.11.0/24', 'fd00::/8']);
  assert.equal(JSON.parse((await validate()).stdout).data.valid, true);
  const home = Object.values(networkInterfaces()).flat()
    .find((entry) => entry.family === 'IPv4' && /^(?:10|192\.168|172\.(?:1[6-9]|2\d|3[01]))\./u.test(entry.address))?.address;
  if (!home) return t.diagnostic('no private address: the screen without a token was not tried');
  config.serve.access.allowed_hosts.push(home);
  const free = await start([home + '/24']);
  assert.equal((await send(free.port, { address: home })).status, 200);
  assert.equal((await send(free.port, {})).status, 200);
});

test('config keeps who may connect, for every project of the machine or for one', { timeout: 120_000 }, async (t) => {
  const root = await realpath(await mkdtemp(join(tmpdir(), 'knowledge-nest-wiki-config-')));
  const stops = [];
  t.after(async () => {
    for (const stop of stops.reverse()) await stop();
    await rm(root, { recursive: true, force: true, maxRetries: 5 });
  });
  const env = { ...process.env, KNOWLEDGE_NEST_GLOBAL_CONFIG: join(root, 'global.yaml') };
  const nest = async (...args) => JSON.parse((await runNode(process.execPath, [cli, ...args, '--json'], { cwd: root, env, encoding: 'utf8' })).stdout).data;
  const refused = (...args) => assert.rejects(nest(...args), (error) => error.code === 2 && JSON.parse(error.stdout).errors[0].code === 'INVALID_ARGUMENT', args.join(' '));
  await runNode(process.execPath, [cli, 'init', 'wiki', 'kb', '--purpose', '設備保全の手順'], { cwd: root, env });
  const configPath = join(root, 'kb', 'KnowledgeNestfile.yaml');
  const written = async () => parse(await readFile(configPath, 'utf8')).serve;
  const standard = await written();
  await writeFile(configPath, stringify({ ...parse(await readFile(configPath, 'utf8')), serve: { ...standard, port: 0 } }));

  // Outside loopback, the settings name the clients of a home or office network, or ask a token.
  await refused('config', 'set', 'serve', '--host', '0.0.0.0');
  await refused('config', 'set', 'serve', '--host', '0.0.0.0', '--allowed-clients', '203.0.113.0/24');
  await refused('config', 'set', 'serve', '--allowed-clients', '192.168.11.*');
  await refused('config', 'set', 'serve', '--auth', 'token');
  await refused('config', 'set', 'serve');
  await refused('config', 'set', 'serve', '--port', 'web');
  await refused('config', 'set', 'serve', '--port', '70000');
  await refused('config', 'set', 'llm', '--model', 'x', '--project', 'kb');
  const own = Object.values(networkInterfaces()).flat()
    .find((entry) => entry.family === 'IPv4' && /^(?:10|192\.168|172\.(?:1[6-9]|2\d|3[01]))\./u.test(entry.address))?.address;
  const hosts = ['localhost', '127.0.0.1', ...(own ? [own] : [])];
  const machine = { host: '0.0.0.0', access: { allowed_hosts: hosts, allowed_clients: ['10.255.255.0/24'] } };
  assert.deepEqual((await nest('config', 'set', 'serve', '--host', '0.0.0.0', '--allowed-hosts', hosts.join(','), '--allowed-clients', '10.255.255.0/24')).serve, machine);
  assert.deepEqual((await nest('config', 'list')).serve, machine);
  assert.match((await runNode(process.execPath, [cli, 'config', 'list'], { cwd: root, env, encoding: 'utf8' })).stdout, /^serve\.allowed_clients {2}10\.255\.255\.0\/24$/mu);
  // The file of the project is not touched: it keeps the standard values, so the machine's apply.
  assert.deepEqual(await written(), { ...standard, port: 0 });

  const start = async () => {
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
  const shared = await start();
  assert.equal((await send(shared.port, {})).status, 200);
  if (own) assert.match((await send(shared.port, { address: own })).body, /CLIENT_FORBIDDEN/u);
  await shared.stop();

  // What the project chooses wins over the machine; the rest still comes from the machine.
  const range = own ? own + '/24' : '10.0.0.0/8';
  const chosen = await nest('config', 'set', 'serve', '--allowed-clients', range, '--project', 'kb');
  assert.equal(chosen.path, configPath);
  assert.deepEqual(chosen.serve.access.allowed_clients, [range]);
  assert.deepEqual(await written(), { ...standard, port: 0, access: { allowed_hosts: standard.access.allowed_hosts, allowed_origins: [], allowed_clients: [range], auth: standard.access.auth } });
  assert.deepEqual(Object.keys((await written()).access), ['allowed_hosts', 'allowed_origins', 'allowed_clients', 'auth']);
  assert.deepEqual((await nest('config', 'list', '--project', 'kb')).serve, { host: '127.0.0.1', port: 0, access: { allowed_hosts: ['localhost', '127.0.0.1'], allowed_clients: [range], auth: { type: 'none', token_env: null } } });
  const narrowed = await start();
  if (own) assert.equal((await send(narrowed.port, { address: own })).status, 200);
  await narrowed.stop();

  // The port of the machine goes to the projects that kept the standard one.
  const free = await new Promise((resolve) => {
    const probe = createServer().listen(0, '127.0.0.1', () => {
      const { port } = probe.address();
      probe.close(() => resolve(port));
    });
  });
  assert.equal((await nest('config', 'set', 'serve', '--port', String(free))).serve.port, free);
  await runNode(process.execPath, [cli, 'init', 'wiki', 'second', '--purpose', '設備保全の手順'], { cwd: root, env });
  const second = spawn(process.execPath, [cli, '--project', 'second', 'serve'], { cwd: root, env, stdio: ['ignore', 'ignore', 'pipe'] });
  const ended = once(second, 'exit');
  stops.push(async () => { second.kill(); await ended; });
  let listening = '';
  for await (const chunk of second.stderr) {
    listening += chunk;
    if (/http:\/\/[\d.]+:\d+/u.test(listening)) break;
  }
  assert.match(listening, new RegExp('http://0\\.0\\.0\\.0:' + free + '\\b', 'u'));
  second.kill();
  await ended;
  assert.equal((await nest('config', 'set', 'serve', '--port', '5001', '--project', 'kb')).serve.port, 5001);
  assert.equal((await written()).port, 5001);
  assert.equal((await nest('config', 'set', 'serve', '--port', '')).serve.port, undefined);

  // Taking the settings away puts the standard values back, in the project and on the machine.
  await nest('config', 'unset', 'serve', '--project', 'kb');
  assert.deepEqual(await written(), standard);
  assert.equal((await nest('config', 'unset', 'serve')).serve, undefined);
  assert.deepEqual(parse(await readFile(join(root, 'global.yaml'), 'utf8')), { version: 1 });
  // A project that binds outside loopback by itself is refused when nothing lets it.
  await refused('config', 'set', 'serve', '--host', '0.0.0.0', '--project', 'kb');
});
