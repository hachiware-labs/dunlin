// A web page whose text a script writes is taken in as the Chrome or Edge of the machine draws
// it, in the browser profile of nest; with --fetch http, as the server sends it. Without the
// profile, or when a page sends the browser to sign in, the page is not read. Skipped on a
// machine without either browser.
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { once } from 'node:events';
import { existsSync } from 'node:fs';
import { createServer } from 'node:http';
import { mkdir, mkdtemp, readFile, realpath, rm, writeFile } from 'node:fs/promises';
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
  // A stand-in for X: its pages draw posts from data they load from /i/api/graphql/.
  const tweet = (id, user, text, urls = [], extra = {}) => ({
    __typename: 'Tweet', rest_id: id,
    core: { user_results: { result: { core: { screen_name: user, name: user.toUpperCase() } } } },
    legacy: { full_text: text, created_at: 'Sun Nov 22 22:50:15 +0000 2026', favorite_count: Number(id), entities: { urls } },
    ...extra,
  });
  const timeline = (tweets) => ({ data: { bookmark_timeline_v2: { timeline: { instructions: [{ entries: tweets.map((result) => ({ content: { itemContent: { tweet_results: { result } } } })) }] } } } });
  const data = {
    '/i/api/graphql/q1/Bookmarks': () => timeline([
      tweet('1', 'alice', '記事です https://t.co/a1', [{ url: 'https://t.co/a1', expanded_url: `${base}/article/one` }], {
        quoted_status_result: { result: tweet('2', 'bob', '引用された投稿') },
      }),
      tweet('3', 'carol', '同じ記事 https://t.co/a2 と会員ページ https://t.co/m', [
        { url: 'https://t.co/a2', expanded_url: `${base}/article/one` },
        { url: 'https://t.co/m', expanded_url: `${base}/members` },
      ]),
    ]),
    '/i/api/graphql/q2/Bookmarks': () => timeline([
      tweet('4', 'dave', 'もう一つ https://t.co/b', [{ url: 'https://t.co/b', expanded_url: `${base}/article/two` }]),
    ]),
  };
  const xPage = (query) => '<!doctype html><html><head><title>ブックマーク / X</title></head><body><div id="app">読み込み中</div><script>'
    + `fetch('${query}').then((r) => r.json()).then(() => { document.getElementById('app').textContent = '投稿'; });`
    + '</script></body></html>';
  const server = createServer((request, response) => {
    if (data[request.url]) {
      response.setHeader('content-type', 'application/json');
      response.end(JSON.stringify(data[request.url]()));
      return;
    }
    if (request.url === '/x/bookmarks' || request.url === '/x/more') {
      response.setHeader('content-type', 'text/html; charset=utf-8');
      response.end(xPage(request.url === '/x/bookmarks' ? '/i/api/graphql/q1/Bookmarks' : '/i/api/graphql/q2/Bookmarks'));
      return;
    }
    if (request.url.startsWith('/article/')) {
      response.setHeader('content-type', 'text/html; charset=utf-8');
      response.end(`<!doctype html><html><head><title>参照先 ${request.url}</title></head><body><p>参照先の本文 ${request.url}</p></body></html>`);
      return;
    }
    if (request.url === '/members') {
      response.writeHead(302, { location: '/account/login?next=/members' });
      response.end();
      return;
    }
    if (request.url.startsWith('/account/login')) {
      response.setHeader('content-type', 'text/html; charset=utf-8');
      response.end('<!doctype html><html><body><form>ログイン</form></body></html>');
      return;
    }
    if (request.url === '/feed') {
      // 30 posts, shown ten at a time: scrolling to the end adds five and drops those far above.
      response.setHeader('content-type', 'text/html; charset=utf-8');
      response.end('<!doctype html><html><head><title>タイムライン</title></head><body><div id="feed"></div><script>'
        + 'const total = 30; let first = 0; let last = 0; const feed = document.getElementById("feed");'
        + 'function render() { feed.style.paddingTop = (first * 300) + "px"; feed.innerHTML = "";'
        + '  for (let i = first; i < last; i++) feed.insertAdjacentHTML("beforeend", `<article style="height:280px;margin:10px 0"><a href="/post/${i}"><time>${i}</time></a><p>投稿${i}の本文</p></article>`); }'
        + 'function more() { if (last >= total) return; last = Math.min(total, last + 5); first = Math.max(0, last - 10); render(); }'
        + 'more(); more();'
        + 'addEventListener("scroll", () => { if (innerHeight + scrollY >= document.documentElement.scrollHeight - 400) setTimeout(more, 100); });'
        + '</script></body></html>');
      return;
    }
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
  const failing = (...args) => run(...args).then((result) => assert.fail(JSON.stringify(result)), (error) => JSON.parse(error.stdout));
  const browser = async (...args) => JSON.parse((await runNode(process.execPath, [cli, 'browser', ...args, '--json'], { cwd: root, env, encoding: 'utf8' })).stdout).data;
  const init = await runNode(process.execPath, [cli, 'init', 'wiki', 'kb', '--purpose', 'アプリの手引き', '--no-llm'], { cwd: root, env, encoding: 'utf8' });
  assert.match(init.stdout, /run nest browser first/u);
  const raw = join(root, 'kb', 'raw', '127.0.0.1');

  // Without the browser profile of nest, no page is read; the profile is beside the user settings.
  const before = await browser('--status');
  assert.equal(before.ready, false);
  assert.equal(before.profile, join(root, 'browser', before.browser.toLowerCase()));
  const unready = await failing('ingest', `${base}/app/faq`, '--source-only');
  assert.deepEqual(unready.errors.map(({ code }) => code), ['BROWSER_NOT_READY']);
  // `nest browser` makes it; here the folder stands for it, as the test opens no window.
  await mkdir(before.profile, { recursive: true });
  assert.equal((await browser('--status')).ready, true);

  // In it, the page opens in the browser, and what its script wrote is kept.
  const drawn = await run('ingest', `${base}/app/faq`, '--source-only');
  assert.equal(drawn.status, 'succeeded', JSON.stringify(drawn.errors));
  assert.match(drawn.data.fetched[0].with, /^(HeadlessChrome|Chrome|Edg|Microsoft Edge)/u);
  assert.match(await readFile(join(raw, 'app', 'faq.html'), 'utf8'), /<h1>描画された見出し<\/h1><p>非同期で届いた本文。<\/p>/u);
  // The browser has closed and left the profile free.
  assert.equal((await browser('--status')).open, false);

  // Scrolled, a page that drops what went out of view is kept as every post it showed, once each.
  const scrolled = await run('ingest', `${base}/feed`, '--source-only', '--scroll', '40');
  assert.equal(scrolled.status, 'succeeded', JSON.stringify(scrolled.errors));
  assert.equal(scrolled.data.fetched[0].scrolled.items, 30);
  assert.ok(scrolled.data.fetched[0].scrolled.steps < 40, 'it stops when nothing more comes');
  assert.deepEqual(scrolled.warnings ?? [], []);
  const feed = await readFile(join(raw, 'feed.html'), 'utf8');
  assert.deepEqual([...feed.matchAll(/投稿(\d+)の本文/gu)].map(([, n]) => Number(n)), [...Array(30).keys()]);
  assert.match(feed, /<title>タイムライン<\/title>/u);
  // Out of scrolls while the page still grows, the posts so far are kept, with a warning.
  const short = await run('ingest', `${base}/feed`, '--source-only', '--scroll', '2');
  assert.ok(short.data.fetched[0].scrolled.items < 30);
  assert.match(JSON.stringify(short.warnings), /raise --scroll/u);
  const invalid = await failing('ingest', `${base}/feed`, '--source-only', '--scroll', 'many');
  assert.deepEqual(invalid.errors.map(({ code }) => code), ['INVALID_ARGUMENT']);

  // A page of X is taken as its posts, one source each, with the page a post links to and the
  // post it quotes, once each; a page it cannot read is told of, and the posts stay.
  const posts = await run('ingest', `${base}/x/bookmarks`, '--source-only');
  assert.equal(posts.status, 'succeeded', JSON.stringify(posts.errors));
  assert.deepEqual(posts.data.fetched.map(({ url, posts: count }) => [url, count]), [[`${base}/x/bookmarks`, 2]]);
  assert.deepEqual(posts.data.followed.map(({ url, from }) => [url, from]), [
    ['https://x.com/bob/status/2', 'https://x.com/alice/status/1'],
    [`${base}/article/one`, 'https://x.com/alice/status/1'],
  ]);
  assert.match(JSON.stringify(posts.warnings), /members, which https:\/\/x\.com\/carol\/status\/3 refers to, was not read: it led to/u);
  const xRaw = join(root, 'kb', 'raw', 'x.com');
  const alice = await readFile(join(xRaw, 'alice', 'status', '1.html'), 'utf8');
  assert.match(alice, /<title>ALICE \(@alice\): 記事です /u);
  assert.match(alice, new RegExp(`<a href="${base}/article/one">`, 'u'));
  assert.match(alice, /<blockquote cite="https:\/\/x\.com\/bob\/status\/2">/u);
  assert.match(await readFile(join(xRaw, 'bob', 'status', '2.html'), 'utf8'), /引用された投稿/u);
  assert.match(await readFile(join(xRaw, 'carol', 'status', '3.html'), 'utf8'), /会員ページ/u);
  assert.match(await readFile(join(raw, 'article', 'one.html'), 'utf8'), /参照先の本文/u);
  // Taken again, the posts and pages that are sources already are not read again.
  const again = await run('ingest', `${base}/x/bookmarks`, '--source-only');
  assert.equal(again.status, 'succeeded', JSON.stringify(again.errors));
  assert.equal(again.data.followed, undefined);
  // With --no-follow, the posts alone; the pages they refer to come when they are taken with it.
  const alone = await run('ingest', `${base}/x/more`, '--source-only', '--no-follow');
  assert.equal(alone.data.followed, undefined);
  assert.equal(existsSync(join(raw, 'article', 'two.html')), false);
  const followedLater = await run('ingest', `${base}/x/more`, '--source-only');
  assert.deepEqual(followedLater.data.followed.map(({ url }) => url), [`${base}/article/two`]);

  // A page that sends the browser to sign in is not read.
  const signedOut = await failing('ingest', `${base}/members`, '--source-only');
  assert.deepEqual(signedOut.errors.map(({ code }) => code), ['BROWSER_SIGNED_OUT']);
  assert.match(signedOut.errors[0].message, /nest browser /u);

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
  const missing = await failing('ingest', `${base}/app/faq`, '--source-only');
  assert.deepEqual(missing.errors.map(({ code }) => code), ['BROWSER_NOT_FOUND']);

  // --reset removes the profile with its sign-ins.
  const reset = await browser('--reset');
  assert.equal(reset.ready, false);
  assert.equal(existsSync(before.profile), false);
});
