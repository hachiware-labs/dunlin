// A Wiki is kept as the LLM Wiki pattern keeps one: a new source is worked into the pages that
// exist, the rules say what kinds of pages there are, the log grows at its end, and the check
// looks for missing links and pages and suggests what to ask next.
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { once } from 'node:events';
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

/** The answer of the test model, from the request alone. */
function answer(request) {
  const input = JSON.parse(request.messages[1].content);
  const properties = request.format.properties;
  if (properties.issues) return { issues: [] };
  if (properties.concepts) {
    return { concepts: [{ name: 'スプリント', pages: input.pages.map(({ item }) => item) }, { name: 'スクラム', pages: [1] }],
      questions: ['スプリントの長さは誰が決めるか'], sources: ['スクラムガイドの最新版'] };
  }
  if (properties.facts) {
    // The number of a source marker for the number of the page, and the marker in the quote, as a small model writes them.
    return { facts: [{ text: '答え。', citations: [{ id: 7, quote: input.pages.at(-1).markdown.split('\n\n')[0] }] }] };
  }
  const about = (word) => input.sources.find(({ text }) => text.includes(word));
  if (about('改訂')) {
    return { pages: [{ id: 'concepts/scrum', title: 'スクラム', facts: [
      // A statement of the page copied with a quote that is nowhere, as a small model does.
      { text: 'スクラムは枠組みである。', citations: [{ id: 1, quote: 'どこにもない引用' }] },
      // The number of the source after the quote, as a small model writes it.
      { text: 'スプリントは2週間が多い。', citations: [{ id: about('改訂').id, quote: 'スプリントは2週間が多い。: 1' }] },
    ] }] };
  }
  if (about('追記')) {
    return { pages: [{ id: 'concepts/scrum', title: 'スクラム', facts: [
      { text: 'デイリースクラムは15分。', citations: [{ id: about('追記').id, quote: 'デイリースクラムは15分。' }] }] }] };
  }
  // The guide: a page of the concept from its two lines, and a page about the guide itself.
  const { id, text } = input.sources[0];
  const [first, second] = text.split('\n');
  return { pages: [
    { id: 'concepts/scrum', title: 'スクラム', facts: [
      { text: first, citations: [{ id, quote: first }] },
      { text: second, citations: [{ id, quote: second }] }] },
    { id: 'sources/guide', title: 'スクラムガイド', facts: [
      { text: 'スクラムガイドは、' + first, citations: [{ id, quote: first }] }] },
  ] };
}

test('works new sources into the pages of a Wiki and keeps its log and its checks', { timeout: 300_000 }, async (t) => {
  const root = await realpath(await mkdtemp(join(tmpdir(), 'knowledge-nest-wiki-integrate-')));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const requests = [];
  const server = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const body = JSON.parse(Buffer.concat(chunks).toString('utf8'));
    requests.push(body);
    response.setHeader('content-type', 'application/json');
    response.end(JSON.stringify({ done: true, message: { content: JSON.stringify(answer(body)) } }));
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  t.after(() => new Promise((resolve) => { server.closeAllConnections(); server.close(resolve); }));

  const env = { ...process.env, KNOWLEDGE_NEST_GLOBAL_CONFIG: join(root, 'global.yaml') };
  const nest = (...args) => runNode(process.execPath, [cli, ...args], { cwd: root, env, encoding: 'utf8', timeout: 60_000 });
  const run = async (...args) => JSON.parse((await nest('--project', 'kb', ...args, '--json')).stdout);
  await nest('init', 'wiki', 'kb', '--purpose', 'スクラムの要点');
  const base = join(root, 'kb');
  const configPath = join(base, 'KnowledgeNestfile.yaml');
  const config = parse(await readFile(configPath, 'utf8'));
  config.models.writer.model = 'testing';
  config.models.writer.base_url = 'http://127.0.0.1:' + server.address().port;
  await writeFile(configPath, stringify(config));
  const sent = (index) => JSON.parse(requests[index].messages[1].content);
  const scrum = (result) => result.data.pages.find(({ id }) => id === 'concepts/scrum').current;

  // The rules a new Wiki starts with: the kinds of pages and what an ingest does.
  const rules = await readFile(join(base, 'rules.md'), 'utf8');
  assert.match(rules, /^# 運用の決まり\n/u);
  for (const part of ['`sources/…`', '`entities/…`', '`concepts/…`', '`overview`', '## 資料を取り込むとき']) assert.ok(rules.includes(part), part);

  // The first source. The model is asked once for every ingest.
  await writeFile(join(root, 'guide.md'), 'スクラムは枠組みである。\nスプリントは1か月以内。\n');
  const first = await run('ingest', join(root, 'guide.md'));
  assert.equal(first.status, 'succeeded', JSON.stringify(first.errors));
  assert.equal(first.command, 'ingress');
  assert.deepEqual(first.data.pages.map(({ id }) => id), ['concepts/scrum', 'sources/guide']);
  assert.equal(requests.length, 1);
  assert.deepEqual(sent(0).pages, []);
  // The model may write several pages with their quotes.
  assert.equal(requests[0].options.num_predict, 8192);
  assert.equal(sent(0).rules, rules);

  // A source that adds to a page. The model is given the list of pages, not what they say, and
  // what it returns for a page that exists is added to the page.
  await writeFile(join(root, 'daily.md'), '追記\nデイリースクラムは15分。\n');
  const added = await run('ingest', join(root, 'daily.md'));
  assert.equal(added.status, 'succeeded', JSON.stringify(added.errors));
  assert.equal(requests.length, 2);
  assert.deepEqual(sent(1).pages, [{ id: 'concepts/scrum', title: 'スクラム' }, { id: 'sources/guide', title: 'スクラムガイド' }]);
  assert.equal(sent(1).sources.length, 1);
  assert.equal(requests[1].messages[0].content, requests[0].messages[0].content);
  assert.equal(scrum(added), 'スクラムは枠組みである。 [1]\n\nスプリントは1か月以内。 [2]\n\nデイリースクラムは15分。 [3]');
  assert.deepEqual(added.warnings, []);

  // What the page says stays, also when the model returns a statement of the page without a
  // quote of the sources; the new statement is added, and its quote is taken without the
  // number the model put after it.
  await writeFile(join(root, 'length.md'), '改訂\nスプリントは2週間が多い。\n');
  const longer = await run('ingest', join(root, 'length.md'));
  assert.equal(longer.status, 'succeeded', JSON.stringify(longer.errors));
  assert.equal(scrum(longer), 'スクラムは枠組みである。 [1]\n\nスプリントは1か月以内。 [2]\n\nデイリースクラムは15分。 [3]\n\nスプリントは2週間が多い。 [4]');
  assert.deepEqual(longer.warnings, []);
  assert.equal((await run('cite', 'concepts/scrum', '4')).status, 'succeeded');

  // A source that changed: the statements that cite its earlier version go, those of the other
  // sources stay, and what the model writes from the new version is added.
  await writeFile(join(root, 'guide.md'), 'スクラムは軽量な枠組みである。\nスプリントは1か月以内。\n');
  const changed = await run('ingest', join(root, 'guide.md'));
  assert.equal(changed.status, 'succeeded', JSON.stringify(changed.errors));
  assert.equal(requests.length, 4);
  assert.deepEqual(sent(3).pages_needing_revision, ['concepts/scrum', 'sources/guide']);
  assert.equal(sent(3).sources.length, 1);
  assert.equal(scrum(changed), 'デイリースクラムは15分。 [1]\n\nスプリントは2週間が多い。 [2]\n\nスクラムは軽量な枠組みである。 [3]\n\nスプリントは1か月以内。 [4]');

  // The check: a page that names another without a link, and with the model a concept without a
  // page and what to ask and to read next.
  const lint = await run('lint');
  assert.equal(lint.command, 'verify');
  const codes = (result) => result.data.issues.map(({ code, page: id, target }) => [code, id, target].filter(Boolean).join(' '));
  assert.deepEqual(codes(lint).sort(), ['MISSING_LINK sources/guide concepts/scrum', 'ORPHAN_PAGE concepts/scrum', 'ORPHAN_PAGE sources/guide']);
  assert.equal(lint.data.issues.find(({ code }) => code === 'MISSING_LINK').description,
    'sources/guide：「スクラム」に触れていますが、そのページ（concepts/scrum）へのリンクがありません');
  const reviewed = await run('lint', '--content');
  assert.equal(reviewed.status, 'succeeded', JSON.stringify(reviewed.errors));
  const missing = reviewed.data.issues.filter(({ code }) => code === 'MISSING_PAGE');
  // スクラム has a page; スプリント is named on one page and has none.
  assert.deepEqual(missing.map(({ concept, pages, description }) => [concept, pages, description]),
    [['スプリント', ['concepts/scrum'], '「スプリント」は concepts/scrum に出てきますが、ページがありません']]);
  assert.deepEqual(reviewed.data.suggestions, { questions: ['スプリントの長さは誰が決めるか'], sources: ['スクラムガイドの最新版'] });
  // The pages did not change, so the model is not asked for suggestions again.
  const asked = requests.length;
  assert.deepEqual((await run('lint', '--content')).data.suggestions, reviewed.data.suggestions);
  assert.equal(requests.length, asked);

  // The log: every work at the start of a line, oldest first, under the names of the pattern.
  const log = await readFile(join(base, 'wiki', 'log.html'), 'utf8');
  const lines = log.split('\n').filter((line) => line.startsWith('<h2>[')).map((line) => line.replace(/^<h2>\[\d{4}-\d\d-\d\d\] |<\/h2>$/gu, ''));
  assert.deepEqual(lines, ['ingest | guide.md', 'ingest | daily.md', 'ingest | length.md', 'ingest | guide.md',
    'lint | 指摘 3 件', 'lint | 指摘 4 件', 'lint | 指摘 4 件']);
  assert.match(log, /ingest \| length\.md<\/h2>\n<p>原本 1 件 · ページ 1 件<\/p>\n<ul><li><a href="concepts\/scrum\.html">スクラム<\/a><\/li><\/ul>/u);
  // A question is a work of the log too, under what was asked.
  const answered = await run('query', 'スプリントの長さ');
  assert.equal(answered.status, 'succeeded', JSON.stringify(answered.errors));
  // The page that holds the quote is the one cited, whatever number the model gave it.
  assert.equal(answered.data.pages.length, 1);
  assert.equal(answered.data.facts[0].evidence.length, 1);
  const later = await readFile(join(base, 'wiki', 'log.html'), 'utf8');
  assert.ok(later.startsWith(log.slice(0, log.indexOf('<!-- knowledge-nest:log-through'))), 'the log only grows at its end');
  assert.match(later, /\n<h2>\[\d{4}-\d\d-\d\d\] query \| スプリントの長さ<\/h2>\n/u);

  // The contents name the kinds of pages and the three operations.
  const contents = await readFile(join(base, 'wiki', 'index.html'), 'utf8');
  assert.match(contents, /<h2>考え方・用語（concepts）<small>1<\/small><\/h2>.*<h2>資料の要約（sources）<small>1<\/small><\/h2>/su);
  for (const name of ['取り込む（ingest）', '質問する（query）', '点検する（lint）', '決まり（schema）', '記録（log）']) assert.ok(contents.includes(name), name);
});
