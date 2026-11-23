// A web page whose text a script writes is taken in as the Chrome or Edge of the machine draws
// it; with --fetch http, as the server sends it. Skipped on a machine without either browser.
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { once } from 'node:events';
import { existsSync } from 'node:fs';
import { createServer } from 'node:http';
import { mkdtemp, readFile, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { parse, stringify } from 'yaml';

const cli = fileURLToPath(new URL('../cli.mjs', import.meta.url));
const runNode = promisify(execFile);

/** Whether the machine has Chrome or Edge where Knowledge-Nest looks for them. */
function browserInstalled() {
  if (process.platform === 'win32') {
    const roots = [process.env.ProgramFiles, process.env['ProgramFiles(x86)'], process.env.LOCALAPPDATA].filter(Boolean);
    return roots.some((root) => existsSync(join(root, 'Google', 'Chrome', 'Application', 'chrome.exe'))
      || existsSync(join(root, 'Microsoft', 'Edge', 'Application', 'msedge.exe')));
  }
  if (process.platform === 'darwin') {
    return ['/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', '/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge'].some(existsSync);
  }
  const directories = (process.env.PATH ?? '').split(':');
  return ['google-chrome', 'google-chrome-stable', 'microsoft-edge', 'chromium', 'chromium-browser']
    .some((name) => directories.some((directory) => existsSync(join(directory, name))));
}

test('takes in a page as the browser draws it', { timeout: 120_000, skip: !browserInstalled() && 'no Chrome or Edge' }, async (t) => {
  const root = await realpath(await mkdtemp(join(tmpdir(), 'knowledge-nest-browser-')));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const server = createServer((request, response) => {
    if (request.url === '/data.json') {
      response.setHeader('content-type', 'application/json');
      setTimeout(() => response.end(JSON.stringify({ body: '非同期で届いた本文。' })), 200);
      return;
    }
    if (request.url === '/notes/guide.md') {
      response.setHeader('content-type', 'text/markdown; charset=utf-8');
      response.end('# 手引き\n\nマークダウンの本文。\n');
      return;
    }
    response.setHeader('content-type', 'text/html; charset=utf-8');
    response.end('<!doctype html><html><head><title>アプリ</title></head><body><div id="app">読み込み中</div><script>'
      + "fetch('/data.json').then((r) => r.json()).then(({ body }) => { document.getElementById('app').innerHTML = '<h1>描画された見出し</h1><p>' + body + '</p>'; });"
      + '</script></body></html>');
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  t.after(() => new Promise((resolve) => { server.closeAllConnections(); server.close(resolve); }));
  const base = `http://127.0.0.1:${server.address().port}`;
  const env = { ...process.env, KNOWLEDGE_NEST_GLOBAL_CONFIG: join(root, 'global.yaml') };
  const run = async (...args) => JSON.parse((await runNode(process.execPath, [cli, '--project', 'kb', ...args, '--json'], { cwd: root, env, encoding: 'utf8' })).stdout);
  await runNode(process.execPath, [cli, 'init', 'wiki', 'kb', '--purpose', 'アプリの手引き', '--no-llm'], { cwd: root, env });
  const raw = join(root, 'kb', 'raw', '127.0.0.1');

  // By default the page opens in the browser, and what its script wrote is kept.
  const drawn = await run('ingest', `${base}/app/faq`, '--source-only');
  assert.equal(drawn.status, 'succeeded', JSON.stringify(drawn.errors));
  assert.match(drawn.data.fetched[0].with, /^(HeadlessChrome|Chrome|Edg|Microsoft Edge)/u);
  assert.match(await readFile(join(raw, 'app', 'faq.html'), 'utf8'), /<h1>描画された見出し<\/h1><p>非同期で届いた本文。<\/p>/u);

  // Downloaded, the page has only what the server sent.
  const downloaded = await run('ingest', `${base}/app/faq`, '--source-only', '--fetch', 'http');
  assert.deepEqual(downloaded.data.fetched, [{ url: `${base}/app/faq`, with: 'http' }]);

  // A file that is no page is downloaded as it is.
  const file = await run('ingest', `${base}/notes/guide.md`, '--source-only');
  assert.deepEqual(file.data.fetched, [{ url: `${base}/notes/guide.md`, with: 'http' }]);
  assert.equal(await readFile(join(raw, 'notes', 'guide.md'), 'utf8'), '# 手引き\n\nマークダウンの本文。\n');

  // The settings choose the way, and the browser.
  const configPath = join(root, 'kb', 'KnowledgeNestfile.yaml');
  const config = parse(await readFile(configPath, 'utf8'));
  config.ingress.modules[0].with.fetch = 'http';
  await writeFile(configPath, stringify(config));
  assert.equal((await run('ingest', `${base}/app/faq`, '--source-only')).data.fetched[0].with, 'http');
  config.ingress.modules[0].with = { ...config.ingress.modules[0].with, fetch: 'browser', browser: join(root, 'no-such-browser.exe') };
  await writeFile(configPath, stringify(config));
  const missing = await run('ingest', `${base}/app/faq`, '--source-only').catch((error) => JSON.parse(error.stdout));
  assert.deepEqual(missing.errors.map(({ code }) => code), ['BROWSER_NOT_FOUND']);
});
