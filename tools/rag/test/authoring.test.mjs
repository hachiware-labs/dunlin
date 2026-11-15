import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, mkdtemp, readFile, readdir, realpath, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { basename, join } from 'node:path';
import { createInterface } from 'node:readline';
import { test } from 'node:test';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { promisify } from 'node:util';
import { deflateSync } from 'node:zlib';

import { Client } from '@modelcontextprotocol/client';
import { StdioClientTransport } from '@modelcontextprotocol/client/stdio';

const cli = fileURLToPath(new URL('./fixtures/legacy-cli.mjs', import.meta.url));
const runNode = promisify(execFile);

async function fixture(t, preset, { wiki = false, web = false, ocrUrl, managedOcr = false, corpusBackend } = {}) {
  const temp = await realpath(await mkdtemp(join(tmpdir(), `dunlin-${preset}-`)));
  const root = join(temp, 'project');
  const config = join(root, 'dunlin.json');
  let client;
  let webUrl;
  t.after(async () => {
    await client?.close();
    await rm(temp, { recursive: true, force: true, maxRetries: 5 });
  });
  await runNode(process.execPath, [cli, 'init', root, '--preset', preset]);
  if (corpusBackend) await runNode(process.execPath, [cli, 'config', 'set', 'corpus.backend', corpusBackend, '--config', config]);
  if (wiki) await runNode(process.execPath, [cli, 'config', 'set', 'authoring.wiki', './wiki', '--config', config]);
  if (ocrUrl) await runNode(process.execPath, [cli, 'setup', 'ocr', '--base-url', ocrUrl, '--config', config]);
  if (managedOcr) await runNode(process.execPath, [cli, 'config', 'set', 'parsers.ocr', '{"tool":"lightonocr"}', '--config', config]);
  async function restart() {
    await client?.close();
    client = new Client({ name: 'authoring-test', version: '1.0.0' });
    const transport = new StdioClientTransport({
      command: process.execPath,
      args: [cli, 'serve', '--stdio', '--config', config, ...(web ? ['--web', '--port', '0'] : [])],
      stderr: web ? 'pipe' : 'inherit',
    });
    const ready = web && once(createInterface({ input: transport.stderr }), 'line');
    await client.connect(transport);
    if (ready) {
      const [message] = await ready;
      assert.match(message, /^Dunlin Web: http:\/\/127\.0\.0\.1:\d+\/$/u);
      webUrl = message.slice('Dunlin Web: '.length);
    }
  }
  await restart();
  async function call(name, args = {}) {
    const result = await client.callTool({ name, arguments: args }, ocrUrl || managedOcr ? { timeout: 180_000 } : undefined);
    assert.ok(!result.isError, JSON.stringify(result.content));
    return result.structuredContent;
  }
  async function reject(name, args, message) {
    const result = await client.callTool({ name, arguments: args });
    assert.equal(result.isError, true);
    assert.match(JSON.stringify(result.content), message);
  }
  return { temp, root, call, reject, restart, get webUrl() { return webUrl; }, readResource: (uri) => client.readResource({ uri }) };
}

test('creates a PARA note through stdio from a URL and local source', { timeout: 30_000 }, async (t) => {
  const originalHtml = '<main><h1>Migration guide</h1><p>Rotate the OAuth client.</p></main>';
  const server = createServer((_request, response) => {
    response.writeHead(200, { 'content-type': 'text/html' }).end(originalHtml);
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  t.after(() => new Promise((resolve) => server.close(resolve)));
  const url = `http://127.0.0.1:${server.address().port}/guide`;
  const { temp, root, call, reject, readResource } = await fixture(t, 'para');
  const local = join(root, 'release.md');
  await writeFile(local, '# Release\nShip the new client in December.\n');
  const outside = join(temp, 'outside.md');
  await writeFile(outside, 'outside');
  await reject('source.ingest', { location: outside }, /outside the configured source root/u);
  const web = await call('source.ingest', { location: url, keepOriginal: true });
  const file = await call('source.ingest', { location: 'release.md' });
  assert.equal(web.document.url, url);
  assert.equal(file.document.url, pathToFileURL(local).href);
  const webLine = await call('source.read', { ...web.document, startLine: 2, maxLines: 1 });
  const fileLine = await call('source.read', { ...file.document, startLine: 2, maxLines: 1 });
  assert.equal(webLine.content, 'Rotate the OAuth client.');
  assert.equal(webLine.evidence.original.uri, web.original.uri);
  assert.equal((await readResource(web.original.uri)).contents[0].blob, Buffer.from(originalHtml).toString('base64'));
  assert.equal(fileLine.content, 'Ship the new client in December.\n');
  assert.equal(fileLine.evidence.original, undefined);
  const draft = {
    id: 'Projects/認証移行/方針', title: '認証移行方針', markdown: '# 方針\n\nOAuth クライアントを更新して12月にリリースする。',
    citations: [webLine.evidence, fileLine.evidence], expectedRevision: null,
  };
  await reject('para.save', { ...draft, id: 'Projects/../escape' }, /safe relative page ID/u);
  const { note } = await call('para.save', draft);
  assert.equal(note.path, join(root, 'para', 'Projects', '認証移行', '方針.html'));
  const content = await readFile(note.path, 'utf8');
  assert.match(content, /^<!doctype html>[\s\S]*<h1>認証移行方針<\/h1>/u);
  assert.ok(content.includes(url));
  assert.ok(content.includes(pathToFileURL(local).href));
  assert.match(content, /L2–L2/u);
  const context = await call('para.context');
  assert.deepEqual(context.groups.Projects, ['認証移行']);
  assert.deepEqual(context.notes, [draft.id]);
  assert.equal((await call('para.context', { id: draft.id })).note.markdown, draft.markdown);
  await reject('para.save', { ...draft, markdown: 'stale overwrite' }, /Revision conflict/u);
  assert.equal(await readFile(note.path, 'utf8'), content);
  const { note: updated } = await call('para.save', { ...draft, expectedRevision: note.revision, markdown: '# 改訂版\n\n12月に公開する。' });
  assert.notEqual(updated.revision, note.revision);
});

test('publishes a Wiki revision and updates all affected pages after the source changes', { timeout: 30_000 }, async (t) => {
  const { root, call, reject } = await fixture(t, 'llm-wiki');
  const file = join(root, 'policy.md');
  await writeFile(file, '# Policy\nSnapshots expire after 30 days.\n');
  const original = await call('source.ingest', { location: 'policy.md' });
  const first = await call('source.read', { ...original.document, maxLines: 1 });
  assert.equal(first.nextLine, 2);
  const excerpt = await call('source.read', { ...original.document, startLine: first.nextLine });
  const drafts = ['バックアップ/保持', '運用'].map((id) => ({
    id, title: id, markdown: '# 初版\n\n保持期間は30日。', citations: [excerpt.evidence], expectedRevision: null,
  }));
  const published = await call('wiki.apply', { pages: drafts, summary: '初版を作成' });
  assert.equal(published.pages.length, 2);
  assert.match(published.indexPath, /index\.html$/u);
  assert.match(await readFile(published.indexPath, 'utf8'), /href="pages\/%E3%83%90[^"]+\.html"/u);
  assert.match(published.logPath, /log\.html$/u);
  assert.match(await readFile(published.logPath, 'utf8'), /初版を作成/u);
  const olderFile = published.pages[0].path;
  const olderText = await readFile(olderFile, 'utf8');
  assert.ok(olderText.includes(pathToFileURL(file).href));

  await writeFile(file, '# Policy\nSnapshots expire after 45 days.\n');
  const current = await call('source.ingest', { location: pathToFileURL(file).href });
  assert.equal(current.document.sourceId, original.document.sourceId);
  assert.notEqual(current.document.version, original.document.version);
  assert.equal((await call('source.read', { ...original.document, startLine: 2 })).content, excerpt.content);
  const impact = await call('wiki.affected');
  assert.deepEqual(impact.affected.map(({ id }) => id), drafts.map(({ id }) => id));
  assert.equal(impact.affected[0].changes[0].reason, 'updated');
  const revised = await call('source.read', { ...current.document, startLine: 2 });
  assert.equal(revised.content, 'Snapshots expire after 45 days.\n');

  const updates = await Promise.all(drafts.map(async (draft) => {
    const { page } = await call('wiki.get', { id: draft.id });
    return { ...draft, expectedRevision: page.revision, markdown: '# 改訂版\n\n保持期間は45日。', citations: [revised.evidence] };
  }));
  await reject('wiki.apply', { pages: [updates[0], { ...updates[1], expectedRevision: null }], summary: '競合' }, /Revision conflict/u);
  await reject('wiki.apply', { pages: [{ ...updates[0], citations: [excerpt.evidence] }], summary: '古い引用' }, /Source object changed/u);
  assert.equal((await call('wiki.get')).revision, published.revision);
  assert.equal(await readFile(olderFile, 'utf8'), olderText);

  const next = await call('wiki.apply', { pages: updates, summary: '保持期間を更新' });
  assert.notEqual(next.revision, published.revision);
  assert.equal((await call('wiki.get')).revision, next.revision);
  assert.match(await readFile(next.pages[0].path, 'utf8'), /45日/u);
  assert.match(await readFile(next.logPath, 'utf8'), /初版を作成[\s\S]*保持期間を更新/u);
  assert.deepEqual((await call('wiki.affected')).affected, []);
});

test('reads legacy Markdown and publishes HTML when a workspace is updated', { timeout: 30_000 }, async (t) => {
  const { root, call } = await fixture(t, 'para', { wiki: true });
  const source = await call('source.upload', { name: 'policy.md', content: '# Policy\nKeep backups for 30 days.' });
  const { evidence } = await call('source.read', { ...source.document, startLine: 2 });
  const markdown = '**30日**保存します。\n\n<script>alert(1)</script>';
  const title = '保存期間';
  const draft = { title, markdown, citations: [evidence] };

  const noteId = 'Resources/policy';
  const oldNotePath = join(root, 'para', 'Resources', 'policy.md');
  await writeFile(oldNotePath, markdown);
  const { note: oldNote } = await call('para.context', { id: noteId });
  assert.equal(oldNote.markdown, markdown);
  const { note } = await call('para.save', { ...draft, id: noteId, expectedRevision: oldNote.revision });
  const html = await readFile(note.path, 'utf8');
  assert.match(html, /<strong>30日<\/strong>/u);
  assert.doesNotMatch(html, /<script/u);
  assert.equal(await readFile(oldNotePath, 'utf8'), markdown);
  assert.deepEqual((await call('para.context')).notes, [noteId]);
  assert.equal((await call('para.context', { id: noteId })).note.path, note.path);

  const token = '00000000-0000-0000-0000-000000000001';
  const legacy = join(root, 'wiki', 'revisions', token);
  await mkdir(join(legacy, 'pages'), { recursive: true });
  await writeFile(join(root, 'wiki', 'CURRENT'), `${token}\n`);
  const pages = ['policy', 'backup'].map((id) => ({ id, title, citations: [evidence] }));
  await writeFile(join(legacy, 'manifest.json'), JSON.stringify(pages));
  await writeFile(join(legacy, 'index.md'), '# Wiki\n');
  await writeFile(join(legacy, 'log.md'), '# 更新履歴\n\n- 旧版を公開\n');
  for (const { id } of pages) await writeFile(join(legacy, 'pages', `${id}.md`), markdown);

  const { page: oldPage } = await call('wiki.get', { id: 'policy' });
  assert.equal(oldPage.markdown, markdown);
  const updated = await call('wiki.apply', {
    pages: [{ ...draft, id: 'policy', expectedRevision: oldPage.revision }], summary: 'HTML で公開',
  });
  assert.match(await readFile(updated.logPath, 'utf8'), /旧版を公開[\s\S]*HTML で公開/u);
  const { page: carried } = await call('wiki.get', { id: 'backup' });
  assert.match(carried.path, /backup\.html$/u);
  assert.equal(carried.markdown, markdown);
  assert.deepEqual(carried.citations, [evidence]);
  assert.equal(await readFile(oldPage.path, 'utf8'), markdown);
});

test('uploads files by value and uses their versioned evidence without a shared path', { timeout: 30_000 }, async (t) => {
  const { root, call, reject, readResource } = await fixture(t, 'llm-wiki');
  const name = '保持方針.md';
  const originalUrl = 'https://example.invalid/retention';
  const raw = Buffer.from('\uFEFF# 保存\r\n保持期間は30日です。\r\n', 'utf8');
  const input = {
    name, originalUrl, encoding: 'base64',
    content: raw.toString('base64'),
  };
  const original = await call('source.upload', input);
  assert.equal(original.document.objectId, name);
  assert.equal(original.document.url, originalUrl);
  assert.equal(original.totalLines, 2);
  assert.equal(original.original.name, name);
  assert.equal(original.original.size, raw.length);
  assert.deepEqual(await readFile(join(root, 'knowledge', '.dunlin-uploads-v1', original.document.sourceId,
    `${original.original.sha256}.bin`)), raw);
  const resource = await readResource(original.original.uri);
  assert.deepEqual(Buffer.from(resource.contents[0].blob, 'base64'), raw);
  assert.deepEqual(await call('source.upload', input), original);
  const excerpt = await call('source.read', { ...original.document, startLine: 2 });
  assert.equal(excerpt.content, '保持期間は30日です。\r\n');
  const published = await call('wiki.apply', {
    pages: [{ id: 'retention', title: '保持期間', markdown: '保持期間は30日です。', citations: [excerpt.evidence], expectedRevision: null }],
    summary: '登録',
  });
  const pageHtml = await readFile(published.pages[0].path, 'utf8');
  assert.ok(pageHtml.includes(`href="${originalUrl}">${name}</a> · L2–L2 · <a href="${original.original.uri}">原本</a>`));

  const current = await call('source.upload', { name, uploadId: original.uploadId, content: '# 保存\n保持期間は45日です。' });
  assert.equal(current.document.sourceId, original.document.sourceId);
  assert.equal(current.document.url, originalUrl);
  assert.notEqual(current.document.version, original.document.version);
  assert.notEqual(current.original.sha256, original.original.sha256);
  assert.equal((await readResource(original.original.uri)).contents[0].blob, input.content);
  await assert.rejects(readResource(`dunlin://original/${original.document.sourceId}/${'0'.repeat(64)}`), /Original file not found/u);
  const historical = await call('source.read', { ...original.document, startLine: 2 });
  assert.equal(historical.content, excerpt.content);
  assert.equal(historical.evidence.original.uri, original.original.uri);
  const impact = await call('wiki.affected');
  assert.deepEqual(impact.affected.map(({ id }) => id), ['retention']);
  const standalone = await call('source.upload', { name, content: 'Other material' });
  assert.notEqual(standalone.document.sourceId, original.document.sourceId);
  assert.equal(standalone.document.url, `dunlin://${standalone.document.sourceId}/${encodeURIComponent(name)}`);

  const html = await call('source.upload', { name: 'notes.html', content: '<main><h1>Guide</h1><p>Keep it.</p></main>' });
  assert.equal((await call('source.read', html.document)).content, 'Guide\nKeep it.');
  await reject('source.upload', { name, content: '%%%', encoding: 'base64' }, /Expected standard Base64/u);
  await reject('source.upload', { name, content: '/w==', encoding: 'base64' }, /utf-8/iu);
  await reject('source.upload', { name: '../private.md', content: 'no' }, /file name without directory/u);
  await reject('source.upload', { name, content: 'x'.repeat(1024 * 1024 + 1) }, /exceeds 1 MiB/u);
  const latest = await call('source.read', { ...current.document, startLine: 2 });
  assert.equal(latest.content, '保持期間は45日です。');
  assert.deepEqual(new Set(latest.originals.map(({ uri }) => uri)), new Set([original.original.uri, current.original.uri]));
  await reject('wiki.apply', {
    pages: [{ id: 'wrong-original', title: '旧版', markdown: '保持期間は45日です。', expectedRevision: null,
      citations: [{ ...latest.evidence, original: { ...latest.evidence.original, sha256: original.original.sha256 } }] }],
    summary: '本文と異なる原本',
  }, /Original file does not match the cited source/u);
});

test('publishes a client attachment to PARA and Wiki and retrieves it from the saved citations', { timeout: 30_000 }, async (t) => {
  const { temp, call, reject, restart, readResource } = await fixture(t, 'para', { wiki: true });
  const attachment = join(temp, '保持方針.md');
  const text = '# Policy\r\nKeep each snapshot for 30 days.\r\n';
  const bytes = Buffer.from(`\uFEFF${text}`);
  await writeFile(attachment, bytes);
  await reject('source.ingest', { location: attachment }, /outside the configured source root/u);
  const name = basename(attachment);
  const original = await call('source.upload', {
    name, encoding: 'base64', content: (await readFile(attachment)).toString('base64'),
  });
  await rm(attachment);
  const { hits } = await call('knowledge.search', { query: 'snapshot', sourceIds: [original.document.sourceId] });
  assert.equal(hits[0].evidence.source.version, original.document.version);
  const { content, evidence } = await call('source.read', { ...hits[0].evidence.source, startLine: 2, maxLines: 1 });
  assert.equal(content, 'Keep each snapshot for 30 days.\r\n');
  assert.equal(evidence.original.uri, original.original.uri);

  const replacement = await call('source.upload', {
    name, uploadId: original.uploadId, encoding: 'base64', content: Buffer.from(text).toString('base64'),
  });
  assert.notEqual(replacement.document.version, original.document.version);
  assert.notEqual(replacement.original.uri, original.original.uri);
  const latest = await call('source.read', { ...replacement.document, startLine: 2 });
  assert.equal(latest.evidence.original.uri, replacement.original.uri);

  const draft = { title: 'スナップショットの保持方針', markdown: 'スナップショットを30日間保持する。',
    citations: [evidence], expectedRevision: null };
  const noteId = 'Areas/運用/保持方針';
  const pageId = '運用/保持方針';
  await call('para.save', { ...draft, id: noteId });
  await call('wiki.apply', { pages: [{ ...draft, id: pageId }], summary: '添付資料から作成' });

  await restart();
  const { note } = await call('para.context', { id: noteId });
  const { page } = await call('wiki.get', { id: pageId });
  for (const artifact of [note, page]) {
    assert.equal(artifact.markdown, draft.markdown);
    assert.match(artifact.path, /\.html$/u);
    const pageHtml = await readFile(artifact.path, 'utf8');
    assert.ok(pageHtml.includes(`href="${original.original.uri}">${name}</a> · L2–L2`));
    const [citation] = artifact.citations;
    assert.equal(citation.original.uri, original.original.uri);
    assert.deepEqual(citation.range, { startLine: 2, endLine: 2 });
    assert.equal((await call('source.read', { ...citation.source, startLine: citation.range.startLine, maxLines: 1 })).content, content);
    const { contents: [{ blob }] } = await readResource(citation.original.uri);
    assert.deepEqual(Buffer.from(blob, 'base64'), bytes);
  }
});

test('serves MCP publications and original downloads across JSON to SQLite migration', { timeout: 30_000 }, async (t) => {
  const project = await fixture(t, 'para', { wiki: true, web: true, corpusBackend: 'json' });
  const { call, root, restart } = project;
  const request = (path, options) => fetch(new URL(path, project.webUrl), options);
  async function html(path, status = 200) {
    const response = await request(path);
    assert.equal(response.status, status, path);
    assert.match(response.headers.get('content-type'), /^text\/html; charset=utf-8$/u);
    return response.text();
  }
  const home = await html('/');
  assert.match(home, /href="\/para">PARA/u);
  assert.match(home, /href="\/wiki">LLM Wiki/u);
  assert.equal((await request('/', { method: 'POST' })).status, 405);
  const head = await request('/', { method: 'HEAD' });
  assert.equal(head.status, 200);
  assert.equal(await head.text(), '');
  await html('/para/Projects/a%2Fb', 404);

  const originalUrl = 'https://example.invalid/retention';
  const raw = Buffer.from('\uFEFF# Policy\r\nKeep snapshots for 30 days.\r\n');
  const first = await call('source.upload', { name: 'policy.md', content: raw.toString('base64'), encoding: 'base64', originalUrl });
  const { evidence } = await call('source.read', { ...first.document, startLine: 2, maxLines: 1 });
  const draft = {
    title: '保持期間', markdown: '## 方針\n\n**30日間**保存します。\n\n<script>alert(1)</script>\n\n[unsafe](javascript:alert%281%29)',
    citations: [evidence], expectedRevision: null,
  };
  const noteId = 'Areas/運用/保持';
  const pageId = '運用/保持';
  const { note } = await call('para.save', { ...draft, id: noteId });
  const { pages: [published] } = await call('wiki.apply', { pages: [{ ...draft, id: pageId }], summary: '初版' });
  const notePath = `/para/${noteId.split('/').map(encodeURIComponent).join('/')}`;
  const wikiPath = `/wiki/${pageId.split('/').map(encodeURIComponent).join('/')}`;
  assert.ok((await html('/para')).includes(`href="${notePath}"`));
  assert.ok((await html('/wiki')).includes(`href="${wikiPath}"`));

  const originalPath = first.original.uri.replace('dunlin://original', '/original');
  const citationPath = (path, revision) => `${path}?citation=1&rev=${revision}`;
  for (const [path, revision] of [[notePath, note.revision], [wikiPath, published.revision]]) {
    const body = await html(path);
    assert.match(body, /<strong>30日間<\/strong>/u);
    assert.doesNotMatch(body, /<script|href="javascript:/u);
    assert.ok(body.includes(`href="${citationPath(path, revision).replace('&', '&amp;')}"`));
    assert.ok(body.includes(`href="${originalPath}" download="policy.md"`));
    const excerpt = await html(citationPath(path, revision));
    assert.match(excerpt, /L2–L2[\s\S]*<pre>Keep snapshots for 30 days\./u);
    assert.ok(excerpt.includes(`href="${originalUrl}"`));
  }

  const second = await call('source.upload', { name: 'policy.md', uploadId: first.uploadId, content: '# Policy\nUpdated.\nKeep snapshots for 52 days.' });
  await runNode(process.execPath, [cli, 'config', 'set', 'corpus.backend', 'sqlite', '--config', join(root, 'dunlin.json')]);
  await restart();
  assert.match(await html(citationPath(notePath, note.revision)), /<pre>Keep snapshots for 30 days\./u);
  assert.match(await html(citationPath(wikiPath, published.revision)), /<pre>Keep snapshots for 30 days\./u);
  const replacement = await call('source.read', { ...second.document, startLine: 3, maxLines: 1 });
  const update = { ...draft, markdown: '**52日間**保存します。', citations: [replacement.evidence] };
  await call('para.save', { ...update, id: noteId, expectedRevision: note.revision });
  const { pages: [revised] } = await call('wiki.apply', {
    pages: [{ ...update, id: pageId, expectedRevision: published.revision }], summary: '改訂',
  });
  await html(citationPath(wikiPath, published.revision), 409);
  assert.match(await html(notePath), /<strong>52日間<\/strong>[\s\S]*L3–L3/u);
  assert.match(await html(citationPath(wikiPath, revised.revision)), /L3–L3[\s\S]*<pre>Keep snapshots for 52 days\./u);
  const download = await request(originalPath);
  assert.equal(download.status, 200);
  assert.equal(download.headers.get('content-type'), 'application/octet-stream');
  assert.equal(download.headers.get('content-disposition'), 'attachment');
  assert.deepEqual(Buffer.from(await download.arrayBuffer()), raw);
});

test('requires preparation before starting the default OCR service', async (t) => {
  const home = await mkdtemp(join(tmpdir(), 'dunlin-ocr-'));
  t.after(() => rm(home, { recursive: true, force: true, maxRetries: 5 }));
  await assert.rejects(runNode(process.execPath, [cli, 'serve-ocr'], {
    env: { ...process.env, DUNLIN_OCR_HOME: home },
  }), { code: 1, stderr: /Run dunlin setup ocr/u });
  assert.deepEqual(await readdir(home), []);
});

test('uses a PDF text layer before trying to start local OCR', async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-ocr-lazy-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const config = join(root, 'dunlin.json');
  const env = { ...process.env, DUNLIN_OCR_HOME: join(root, 'ocr') };
  await runNode(process.execPath, [cli, 'init', root]);
  await runNode(process.execPath, [cli, 'config', 'set', 'parsers.ocr', '{"tool":"lightonocr"}', '--config', config]);
  const file = join(root, 'text.pdf');
  await writeFile(file, textLayerPdf('Text layer'));
  const { stdout } = await runNode(process.execPath, [cli, 'ingest', '--config', config, '--source', 'text', file], { env });
  assert.equal(JSON.parse(stdout).totalLines, 1);
  const scan = join(root, 'scan.pdf');
  await writeFile(scan, await scannedPdf(['Scan']));
  await assert.rejects(runNode(process.execPath, [cli, 'ingest', '--config', config, '--source', 'scan', scan], { env }),
    { code: 1, stderr: /Run dunlin setup ocr/u });
});

test('builds mixed PDF and text sources in stable order and preserves the active build on failure', async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-mixed-sources-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const projectCli = fileURLToPath(new URL('../cli.mjs', import.meta.url));
  const env = { ...process.env, DUNLIN_GLOBAL_CONFIG: join(root, 'global.yaml') };
  const json = async (...args) => JSON.parse((await runNode(process.execPath, [projectCli, ...args, '--json'],
    { cwd: root, env, timeout: 15_000 })).stdout);
  await json('init', 'rag', '.');
  const docs = join(root, 'docs');
  const pdf = textLayerPdf('Calibration procedure', 'Replace the filter every six months.');
  await Promise.all([
    writeFile(join(docs, 'a-original.pdf'), pdf),
    writeFile(join(docs, 'b-duplicate.pdf'), pdf),
    writeFile(join(docs, 'c-empty.md'), ' '),
    writeFile(join(docs, 'd-empty.txt'), ''),
    writeFile(join(docs, 'e-guide.md'), '# 点検手順\n電源を止めます。\n'),
    writeFile(join(docs, 'ignored.bin'), 'unsupported'),
  ]);
  const build = await json('build', '--skip-unsupported');
  assert.equal(build.data.documents, 4);
  assert.deepEqual(build.warnings.filter(({ code }) => code === 'EMPTY_EXTRACTION').map(({ target }) => target),
    ['docs:c-empty.md', 'docs:d-empty.txt']);
  const search = (await json('search', 'Calibration')).data;
  assert.deepEqual(search.results.map(({ document_id }) => document_id).sort(), ['docs:a-original.pdf', 'docs:b-duplicate.pdf']);
  assert.ok(search.results.every(({ page }) => page === 1));
  const originals = join(root, '.dunlin', 'artifacts', 'originals', 'shared');
  const archived = await Promise.all((await readdir(originals)).map((name) => readFile(join(originals, name))));
  assert.equal(archived.filter((bytes) => bytes.equals(pdf)).length, 1);
  const repeated = await json('build', '--skip-unsupported');
  assert.equal(repeated.data.reused, true);
  for (const [stage, result] of Object.entries(repeated.data.stages)) {
    assert.equal(result.id, build.data.stages[stage].id);
    assert.equal(result.reused, true);
    assert.ok(Number.isInteger(result.elapsed_ms) && result.elapsed_ms >= 0);
  }

  await writeFile(join(docs, 'a-broken.pdf'), '%PDF-1.4\ninvalid');
  await rm(join(docs, 'b-duplicate.pdf'));
  await writeFile(join(docs, 'e-guide.md'), '# 点検手順\n新しい手順です。\n');
  const changes = (await json('build', '--dry-run')).data.files.map(({ path, change }) => [path, change]);
  assert.deepEqual(changes.filter(([, change]) => change !== 'unchanged'),
    [['a-broken.pdf', 'added'], ['e-guide.md', 'updated'], ['ignored.bin', 'added'], ['b-duplicate.pdf', 'deleted']]);
  await assert.rejects(json('build', '--skip-unsupported'), (failure) => {
    assert.equal(failure.code, 1);
    assert.match(JSON.parse(failure.stdout).errors[0].message, /Could not load docs:a-broken\.pdf/u);
    return true;
  });
  assert.deepEqual((await json('search', 'Calibration')).data, search);
});

test('uses text-layer PDFs in an OCR-enabled project and guides setup for scans', async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-project-ocr-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const projectCli = fileURLToPath(new URL('../cli.mjs', import.meta.url));
  const env = { ...process.env, DUNLIN_GLOBAL_CONFIG: join(root, 'global.yaml'), DUNLIN_OCR_HOME: join(root, 'ocr') };
  const run = (...args) => runNode(process.execPath, [projectCli, ...args], { cwd: root, env, timeout: 15_000 });
  await run('config', 'set', 'ocr');
  await run('init', 'rag', '.');
  await writeFile(join(root, 'docs', 'manual.pdf'), textLayerPdf('Calibration procedure'));
  await run('build');
  const { stdout } = await run('search', 'Calibration', '--json');
  const [result] = JSON.parse(stdout).data.results;
  assert.equal(result.document_id, 'docs:manual.pdf');
  assert.equal(result.page, 1);

  await writeFile(join(root, 'docs', 'scan.pdf'), await scannedPdf(['Scan']));
  await assert.rejects(run('build'), { code: 1, stderr: /Run dunlin add ocr\./u });
});

test('shares Docling across projects and converts locally after the service stops', { timeout: 120_000 }, async (t) => {
  try {
    await import('docling.rs');
  } catch (error) {
    if (process.env.DUNLIN_TEST_DOCLING_MODEL_DIR) throw error;
    t.skip('docling.rs native package is unavailable');
    return;
  }
  const modelDir = process.env.DUNLIN_TEST_DOCLING_MODEL_DIR;
  const server = spawn(process.execPath, [cli, 'serve-docling', '--port', '0', ...(modelDir ? ['--model-dir', modelDir] : [])], {
    stdio: ['ignore', 'pipe', 'inherit'],
  });
  const stopped = once(server, 'close');
  async function stop() {
    server.kill();
    await stopped;
  }
  t.after(stop);
  const lines = createInterface({ input: server.stdout });
  const [message] = await Promise.race([
    once(lines, 'line', { signal: AbortSignal.timeout(30_000) }),
    stopped.then(([code]) => { throw new Error(`Docling server exited before startup (${code})`); }),
  ]);
  assert.match(message, /^Dunlin Docling: http:\/\/127\.0\.0\.1:\d+$/u);
  const baseUrl = message.slice('Dunlin Docling: '.length);

  const projects = [];
  for (const title of ['Project Alpha', 'Project Beta']) {
    const { root, call, readResource } = await fixture(t, 'para');
    const config = join(root, 'dunlin.json');
    const run = (...args) => runNode(process.execPath, [cli, ...args, '--config', config]);
    const raw = textLayerPdf(title, 'This report records the approved operating procedure.');
    const file = join(root, 'document.pdf');
    await writeFile(file, raw);
    const fallbackModels = title === 'Project Alpha' && modelDir ? modelDir : './local-docling';
    await run('config', 'set', 'parsers.documents', JSON.stringify({ tool: 'docling-rs', modelDir: fallbackModels }));
    const { stdout } = await run('setup', 'documents', '--tool', 'docling-serve', '--base-url', baseUrl);
    assert.equal(JSON.parse(stdout).results[0].execution, 'shared');
    assert.equal(JSON.parse(await readFile(config, 'utf8')).parsers.documents.modelDir, fallbackModels);
    const { stdout: uploaded } = await run('ingest', '--source', 'pdf', file);
    const { document, original } = JSON.parse(uploaded);
    const read = await call('source.read', document);
    assert.ok(read.content.includes(title), JSON.stringify(read.content));
    assert.deepEqual(Buffer.from((await readResource(original.uri)).contents[0].blob, 'base64'), raw);

    projects.push({ run, file, call, title, fallbackModels });
  }

  const [, beta] = projects;
  await beta.run('config', 'set', 'parsers.documents.baseUrl', `${baseUrl}/missing`);
  await assert.rejects(beta.run('ingest', '--source', 'invalid', beta.file), { code: 1, stderr: /HTTP 404/u });
  await beta.run('config', 'set', 'parsers.documents.baseUrl', baseUrl);
  await stop();
  for (const { run, file, call, title, fallbackModels } of projects) {
    const { stdout: health } = await run('setup', 'documents', '--check');
    const { execution, layoutModelsReady } = JSON.parse(health).results[0];
    assert.equal(execution, 'local');
    assert.equal(layoutModelsReady, fallbackModels === modelDir);
    const { stdout: fallback } = await run('ingest', '--source', 'offline', file);
    assert.ok((await call('source.read', JSON.parse(fallback).document)).content.includes(title));
  }
});

test('sets up LightOnOCR and publishes a scanned PDF with a highlighted original page', { timeout: 240_000 }, async (t) => {
  const model = 'lightonai/LightOnOCR-3-1B';
  const live = process.env.DUNLIN_TEST_LIGHTON_URL;
  const managedOcr = !live && process.env.DUNLIN_TEST_MANAGED_OCR === '1';
  const text = '保存期間は52日です。';
  const accelerator = { device: 'gpu', deviceName: 'Test Radeon', runtime: 'ROCm', dtype: 'bfloat16' };
  let calls = 0;
  let rejectConversion = false;
  const server = createServer(async (request, response) => {
    response.setHeader('Content-Type', 'application/json');
    if (request.url === '/v1/models') {
      response.end(JSON.stringify({ data: [{ id: model, ...accelerator }] }));
      return;
    }
    assert.equal(request.url, '/v1/chat/completions');
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const input = JSON.parse(Buffer.concat(chunks));
    assert.equal(input.model, model);
    assert.equal(input.messages[0].content[1].text, 'grounding');
    assert.match(input.messages[0].content[0].image_url.url, /^data:image\/png;base64,iVBORw0KGgo/u);
    if (rejectConversion) {
      response.writeHead(401);
      response.end('{}');
      return;
    }
    const output = ++calls % 2 === 1 ? '運用手順' : text;
    response.end(JSON.stringify({ choices: [{ message: { content: `![text](120,230,860,420)\n${output}` } }] }));
  });
  let ocrUrl = live;
  if (!ocrUrl && !managedOcr) {
    server.listen(0, '127.0.0.1');
    await once(server, 'listening');
    t.after(() => new Promise((resolve) => server.close(resolve)));
    ocrUrl = `http://127.0.0.1:${server.address().port}/v1`;
  }
  const { root, call, webUrl, readResource } = await fixture(t, 'para', { wiki: true, web: true, ocrUrl, managedOcr });
  const config = join(root, 'dunlin.json');
  const before = await readFile(config, 'utf8');
  assert.equal(JSON.parse(before).parsers.ocr.tool, 'lightonocr');
  const { stdout } = await runNode(process.execPath, [cli, 'setup', 'ocr', '--check', '--config', config]);
  const { category, tool, configured, ready, model: selected, lang, device, deviceName, runtime, dtype, execution } = JSON.parse(stdout).results[0];
  assert.deepEqual({ category, tool, configured, ready, model: selected, lang },
    { category: 'ocr', tool: 'lightonocr', configured: true, ready: true, model, lang: 'ja' });
  if (!live && !managedOcr) assert.deepEqual({ device, deviceName, runtime, dtype }, accelerator);
  if (managedOcr) assert.equal(device, 'gpu');
  assert.equal(execution, managedOcr ? 'local' : 'shared');
  assert.equal(await readFile(config, 'utf8'), before);
  const raw = await scannedPdf(['運用手順', text]);
  const result = await call('source.upload', { name: 'scan.pdf', encoding: 'base64', content: raw.toString('base64') });
  if (!live && !managedOcr) assert.equal(calls, 2);
  const full = await call('source.read', result.document);
  const target = full.content.split('\n').findIndex((line) => line.replace(/\s/gu, '').includes(text)) + 1;
  assert.ok(target > 0, `Expected Japanese scan text in ${JSON.stringify(full.content)}`);
  const quote = await call('source.read', { ...result.document, startLine: target, maxLines: 1 });
  const block = quote.evidence.locations.find(({ page, bbox }) => page === 2 && bbox?.length === 4);
  assert.ok(block);
  assert.deepEqual(Buffer.from((await readResource(result.original.uri)).contents[0].blob, 'base64'), raw);
  if (!live) {
    const local = join(root, 'scan.pdf');
    await writeFile(local, raw);
    const { stdout: imported } = await runNode(process.execPath, [cli, 'ingest', '--config', config, '--source', 'scanner', local]);
    const { document, original } = JSON.parse(imported);
    assert.equal(document.sourceId, 'scanner');
    const { evidence } = await call('source.read', { ...document, startLine: 2, maxLines: 1 });
    assert.equal(evidence.locations[0].page, 2);
    assert.equal(evidence.original.uri, original.uri);
    assert.deepEqual(Buffer.from((await readResource(original.uri)).contents[0].blob, 'base64'), raw);
  }
  const draft = { title: '保存期間', markdown: `> ${text}`, citations: [quote.evidence], expectedRevision: null };
  const { note } = await call('para.save', { ...draft, id: 'Resources/scan' });
  const { pages: [wiki] } = await call('wiki.apply', { pages: [{ ...draft, id: 'scan' }], summary: 'スキャンの引用' });
  for (const [path, revision] of [['/para/Resources/scan', note.revision], ['/wiki/scan', wiki.revision]]) {
    const citation = `${path}?citation=1&rev=${revision}`;
    const page = await fetch(new URL(citation, webUrl));
    assert.equal(page.status, 200);
    assert.match(await page.text(), /原本 p\. 2/u);
    const preview = await fetch(new URL(`${citation}&page=2`, webUrl));
    assert.equal(preview.status, 200);
    assert.equal(preview.headers.get('content-type'), 'image/png');
    const { createCanvas, loadImage } = await import('@napi-rs/canvas');
    const image = await loadImage(Buffer.from(await preview.arrayBuffer()));
    const canvas = createCanvas(image.width, image.height);
    const context = canvas.getContext('2d');
    context.drawImage(image, 0, 0);
    const [left, top, right] = block.bbox;
    const x = Math.floor((left + right) * image.width / 2000);
    const y = Math.max(0, Math.floor(top * image.height / 1000));
    const [red, green, blue] = context.getImageData(x, y, 1, 1).data;
    assert.ok(red > 160 && green < 100 && blue < 100, 'Expected a red highlight on the cited bounding box');
  }
  if (!live && !managedOcr) {
    const args = [cli, 'ingest', '--config', config, '--source', 'scanner', join(root, 'scan.pdf')];
    const env = { ...process.env, DUNLIN_OCR_HOME: join(root, 'unprepared-ocr') };
    rejectConversion = true;
    await assert.rejects(runNode(process.execPath, args, { env }), { code: 1, stderr: /LightOnOCR chat\/completions returned HTTP 401/u });
    await new Promise((resolve) => server.close(resolve));
    await assert.rejects(runNode(process.execPath, args, { env }), { code: 1, stderr: /Local GPU OCR failed: .*Run dunlin setup ocr/u });
  }
});

async function scannedPdf(lines) {
  const { createCanvas } = await import('@napi-rs/canvas');
  const width = 720;
  const height = 360;
  const objects = ['<< /Type /Catalog /Pages 2 0 R >>',
    `<< /Type /Pages /Count ${lines.length} /Kids [${lines.map((_, index) => `${3 + 3 * index} 0 R`).join(' ')}] >>`];
  for (const line of lines) {
    const canvas = createCanvas(width, height);
    const context = canvas.getContext('2d');
    context.fillStyle = 'white';
    context.fillRect(0, 0, width, height);
    context.fillStyle = 'black';
    context.font = '42px "Yu Gothic", Meiryo, sans-serif';
    context.fillText(line, 82, 145);
    const pixels = context.getImageData(0, 0, width, height).data;
    const rgb = Buffer.alloc(width * height * 3);
    for (let i = 0, j = 0; i < pixels.length; i += 4, j += 3) {
      rgb[j] = pixels[i];
      rgb[j + 1] = pixels[i + 1];
      rgb[j + 2] = pixels[i + 2];
    }
    const image = deflateSync(rgb);
    const index = objects.length + 1;
    objects.push(`<< /Type /Page /Parent 2 0 R /MediaBox [0 0 ${width} ${height}] /Resources << /XObject << /Im ${index + 1} 0 R >> >> /Contents ${index + 2} 0 R >>`,
      Buffer.concat([Buffer.from(`<< /Type /XObject /Subtype /Image /Width ${width} /Height ${height} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode /Length ${image.length} >>\nstream\n`), image, Buffer.from('\nendstream')]),
      `<< /Length ${String(`q ${width} 0 0 ${height} 0 0 cm /Im Do Q`).length} >>\nstream\nq ${width} 0 0 ${height} 0 0 cm /Im Do Q\nendstream`);
  }
  return pdfWithObjects(objects);
}

function textLayerPdf(title, body) {
  const stream = `BT /F1 24 Tf 72 700 Td (${title}) Tj ET`
    + (body ? `\nBT /F1 12 Tf 72 650 Td (${body}) Tj ET` : '');
  return pdfWithObjects([
    '<< /Type /Catalog /Pages 2 0 R >>', '<< /Type /Pages /Count 1 /Kids [3 0 R] >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>',
    '<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>', `<< /Length ${stream.length} >>\nstream\n${stream}\nendstream`,
  ]);
}

function pdfWithObjects(objects) {
  const pdf = [Buffer.from('%PDF-1.4\n')];
  const offsets = [];
  let offset = pdf[0].length;
  for (const [index, object] of objects.entries()) {
    offsets.push(offset);
    const chunk = Buffer.concat([Buffer.from(`${index + 1} 0 obj\n`), Buffer.from(object), Buffer.from('\nendobj\n')]);
    pdf.push(chunk);
    offset += chunk.length;
  }
  pdf.push(Buffer.from(`xref\n0 ${objects.length + 1}\n0000000000 65535 f \n${offsets.map((value) => `${String(value).padStart(10, '0')} 00000 n \n`).join('')}trailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\nstartxref\n${offset}\n%%EOF\n`));
  return Buffer.concat(pdf);
}
