// A Wiki is kept as the LLM Wiki pattern keeps one: a new source is worked into the pages that
// exist, the rules say what kinds of pages there are, the log grows at its end, and the check
// looks for missing links and pages and suggests what to ask next.
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { mkdtemp, readdir, readFile, realpath, rm, writeFile } from 'node:fs/promises';
import { DatabaseSync } from 'node:sqlite';
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
      questions: ['スプリントの長さは誰が決めるか'], sources: ['スクラムガイドの最新版'],
      // The summary of the guide fits another category of the rules.
      misplaced: [{ item: input.pages.findIndex(({ id }) => id === 'sources/guide') + 1, category: '資料' }] };
  }
  if (properties.facts) {
    // The number of a source marker for the number of the page, and the marker in the quote, as a small model writes them.
    return { facts: [{ text: '答え。', citations: [{ id: 7, quote: input.pages.at(-1).markdown.split('\n\n')[0] }] }] };
  }
  const about = (word) => input.sources.find(({ text }) => text.includes(word));
  // A source with nothing for the wiki: no pages.
  if (about('天気')) return { pages: [] };
  if (about('改訂')) {
    // A paragraph the page has already, copied as a small model does, then the new one, with
    // the source marker it put after it.
    return { pages: [{ id: 'concepts/scrum', title: 'スクラム', category: '別の分類',
      markdown: 'スクラムは枠組みである。\n\nスプリントは2週間が多い。 [1]', sources: [about('改訂').id] }] };
  }
  if (about('追記')) {
    return { pages: [{ id: 'concepts/scrum', title: 'スクラム', category: '別の分類',
      markdown: 'デイリースクラムは15分。', sources: [about('追記').id] }] };
  }
  // The guide: a page of the concept from its two lines, and a page about the guide itself. A
  // page given in full, as the guide changed, comes back whole with the new wording.
  const { id, text } = input.sources[0];
  const [first, second] = text.split('\n');
  const given = (page) => input.pages.find((item) => item.id === page)?.markdown;
  return { pages: [
    { id: 'concepts/scrum', title: 'スクラム', category: 'スクラム', sources: [id],
      markdown: given('concepts/scrum')?.replace('スクラムは枠組みである。', first) ?? `${first}\n\n${second}` },
    { id: 'sources/guide', title: 'スクラムガイド', category: 'スクラム', sources: [],
      markdown: given('sources/guide')?.replace('スクラムは枠組みである。', first) ?? 'スクラムガイドは、' + first },
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
  // A new Wiki has the folders of the pattern: raw/ for the sources, wiki/ for the pages.
  assert.deepEqual((await readdir(base)).filter((name) => !name.startsWith('.')).sort(), ['KnowledgeNestfile.yaml', 'raw', 'rules.md', 'wiki']);
  const rules = await readFile(join(base, 'rules.md'), 'utf8');
  assert.match(rules, /^# 運用の決まり\n/u);
  for (const part of ['## 分類', '`要約-`', '## 資料を取り込むとき', 'フォルダを付けない名前', '全体は目録']) assert.ok(rules.includes(part), part);
  assert.ok(!rules.includes('overview'), 'no page about everything');

  // The first source. The model is asked once for every ingest.
  await writeFile(join(root, 'guide.md'), 'スクラムは枠組みである。\nスプリントは1か月以内。\n');
  const first = await run('ingest', join(root, 'guide.md'));
  assert.equal(first.status, 'succeeded', JSON.stringify(first.errors));
  assert.equal(first.command, 'ingress');
  // A source from outside raw/ is copied into raw/ and read from there.
  assert.equal(await readFile(join(base, 'raw', 'guide.md'), 'utf8'), 'スクラムは枠組みである。\nスプリントは1か月以内。\n');
  const cited = JSON.parse(/id="knowledge-nest-page">(.*?)<\/script>/su.exec(await readFile(join(base, 'wiki', 'concepts', 'scrum.html'), 'utf8'))[1]);
  // A page is its Markdown and the sources it is written from.
  assert.equal(cited.markdown, 'スクラムは枠組みである。\n\nスプリントは1か月以内。');
  assert.equal(cited.citations, undefined);
  assert.equal(cited.sources.length, 1);
  assert.match(cited.sources[0].source.url, /\/kb\/raw\/guide\.md$/u);
  // A page that names no source is written from those the model read.
  const guidePage = JSON.parse(/id="knowledge-nest-page">(.*?)<\/script>/su.exec(await readFile(join(base, 'wiki', 'sources', 'guide.html'), 'utf8'))[1]);
  assert.deepEqual(guidePage.sources.map(({ source }) => source.objectId), ['guide.md']);
  assert.deepEqual(first.data.pages.map(({ id }) => id), ['concepts/scrum', 'sources/guide']);
  assert.equal(requests.length, 1);
  assert.deepEqual(sent(0).pages, []);
  // The model is told the name of each source, to name its summary page after it.
  assert.deepEqual(sent(0).sources.map(({ id, name }) => [id, name]), [[1, 'guide.md']]);
  // The model writes as much as it needs: no limit is sent.
  assert.deepEqual(requests[0].options, { temperature: 0 });
  assert.equal(sent(0).rules, rules);

  // A source that adds to a page. The model is given the list of pages, not what they say, and
  // what it returns for a page that exists is added to the page.
  await writeFile(join(root, 'daily.md'), '追記\nデイリースクラムは15分。\n');
  const added = await run('ingest', join(root, 'daily.md'));
  assert.equal(added.status, 'succeeded', JSON.stringify(added.errors));
  assert.equal(requests.length, 2);
  assert.deepEqual(sent(1).pages, [{ id: 'concepts/scrum', title: 'スクラム' }, { id: 'sources/guide', title: 'スクラムガイド' }]);
  assert.equal(sent(1).sources.length, 1);
  assert.match(requests[1].messages[0].content, /For an existing page, return only the new text to add to it\./u);
  assert.doesNotMatch(requests[0].messages[0].content, /existing page/u);
  assert.equal(scrum(added), 'スクラムは枠組みである。\n\nスプリントは1か月以内。\n\nデイリースクラムは15分。');
  assert.deepEqual(added.warnings, []);

  // What the page says stays; of what the model returns, only what the page does not say yet is
  // added, without the source markers it wrote.
  await writeFile(join(root, 'length.md'), '改訂\nスプリントは2週間が多い。\n');
  const longer = await run('ingest', join(root, 'length.md'));
  assert.equal(longer.status, 'succeeded', JSON.stringify(longer.errors));
  assert.equal(scrum(longer), 'スクラムは枠組みである。\n\nスプリントは1か月以内。\n\nデイリースクラムは15分。\n\nスプリントは2週間が多い。');
  assert.deepEqual(longer.warnings, []);
  // The sources of the page, in the order they were added; cite gives one of them.
  const cite = await run('cite', 'concepts/scrum', '3');
  assert.equal(cite.status, 'succeeded', JSON.stringify(cite.errors));
  assert.equal(cite.data.evidence.source.objectId, 'length.md');
  assert.equal(cite.data.content, '改訂\nスプリントは2週間が多い。\n');

  // A source that changed: the pages written from it are given in full and come back whole.
  await writeFile(join(root, 'guide.md'), 'スクラムは軽量な枠組みである。\nスプリントは1か月以内。\n');
  const changed = await run('ingest', join(root, 'guide.md'));
  assert.equal(changed.status, 'succeeded', JSON.stringify(changed.errors));
  // The changed file is a new copy beside the first, which stays as it was, and a new version of
  // the same source.
  assert.deepEqual(changed.data.source_ids, first.data.source_ids);
  const copies = (await readdir(join(base, 'raw'))).filter((name) => name.startsWith('guide')).sort();
  assert.equal(copies.length, 2);
  assert.match(copies[0], /^guide-[0-9a-f]{8}\.md$/u);
  assert.equal(await readFile(join(base, 'raw', 'guide.md'), 'utf8'), 'スクラムは枠組みである。\nスプリントは1か月以内。\n');
  assert.equal(requests.length, 4);
  assert.deepEqual(sent(3).pages_needing_revision, ['concepts/scrum', 'sources/guide']);
  assert.equal(sent(3).sources.length, 1);
  assert.equal(sent(3).pages[0].markdown, scrum(longer));
  assert.match(requests[3].messages[0].content, /return the whole page, revised with what the sources add or change/u);
  assert.equal(scrum(changed), 'スクラムは軽量な枠組みである。\n\nスプリントは1か月以内。\n\nデイリースクラムは15分。\n\nスプリントは2週間が多い。');
  // The page is written from the new version of the source, in its place, and the others.
  const sourcesOf = async (file) => JSON.parse(/id="knowledge-nest-page">(.*?)<\/script>/su.exec(await readFile(file, 'utf8'))[1]).sources;
  const now = await sourcesOf(join(base, 'wiki', 'concepts', 'scrum.html'));
  assert.deepEqual(now.map(({ source }) => source.objectId), ['guide.md', 'daily.md', 'length.md']);
  assert.notEqual(now[0].source.version, cited.sources[0].source.version);

  // A source that adds nothing to the wiki is recorded as such, and no page changes.
  await writeFile(join(root, 'other.md'), '天気の話。\n');
  const nothing = await run('ingest', join(root, 'other.md'));
  assert.equal(nothing.status, 'succeeded', JSON.stringify(nothing.errors));
  assert.deepEqual(nothing.data.pages, []);
  assert.deepEqual(nothing.data.no_material, ['other.md']);

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
  assert.deepEqual(lines, ['ingest | guide.md', 'ingest | daily.md', 'ingest | length.md', 'ingest | guide.md', 'ingest | other.md',
    'lint | 指摘 3 件', 'lint | 指摘 4 件', 'lint | 指摘 4 件']);
  assert.match(log, /ingest \| other\.md<\/h2>\n<p>原本 1 件 · 得るものなし：other\.md<\/p>/u);
  assert.match(log, /ingest \| length\.md<\/h2>\n<p>原本 1 件 · ページ 1 件<\/p>\n<ul><li><a href="concepts\/scrum\.html">スクラム<\/a><\/li><\/ul>/u);
  // A question is a work of the log too, under what was asked.
  const answered = await run('query', 'スプリントの長さ');
  assert.equal(answered.status, 'succeeded', JSON.stringify(answered.errors));
  // The page that holds the quote is the one cited, whatever number the model gave it.
  assert.equal(answered.data.pages.length, 1);
  // Its evidence is the sources of that page.
  assert.deepEqual(answered.data.facts[0].evidence, answered.data.pages[0].sources);
  const later = await readFile(join(base, 'wiki', 'log.html'), 'utf8');
  assert.ok(later.startsWith(log.slice(0, log.indexOf('<!-- knowledge-nest:log-through'))), 'the log only grows at its end');
  assert.match(later, /\n<h2>\[\d{4}-\d\d-\d\d\] query \| スプリントの長さ<\/h2>\n/u);

  // The pages are the files of the folder. Each holds its page as JSON, and the state keeps none.
  const state = () => new DatabaseSync(join(base, '.knowledge-nest', 'state.sqlite'));
  const db = state();
  assert.equal(db.prepare('SELECT count(*) AS n FROM wiki_pages').get().n, 0);
  db.close();
  const scrumFile = join(base, 'wiki', 'concepts', 'scrum.html');
  const record = (html) => JSON.parse(/<script type="application\/json" id="knowledge-nest-page">(.*?)<\/script>/su.exec(html)[1]);
  const html = await readFile(scrumFile, 'utf8');
  assert.equal(record(html).markdown, scrum(changed));
  // A change to the file is a change to the page.
  await writeFile(scrumFile, html.replace('"title":"スクラム"', '"title":"スクラム（枠組み）"'));
  await run('lint');
  assert.match(await readFile(join(base, 'wiki', 'index.html'), 'utf8'), /<strong>スクラム（枠組み）<\/strong>/u);
  // The pages of an earlier state, kept in its table, are read from there until the next work
  // moves them into their files.
  const earlier = { ...record(html), id: 'concepts/earlier', title: '前の形のページ' };
  const old = state();
  old.prepare('INSERT INTO wiki_pages VALUES (?, ?)').run(earlier.id, JSON.stringify(earlier));
  old.close();
  assert.ok((await run('lint')).data.issues.some(({ page }) => page === 'concepts/earlier'));
  await run('ingest', join(root, 'daily.md'));
  assert.equal(record(await readFile(join(base, 'wiki', 'concepts', 'earlier.html'), 'utf8')).title, '前の形のページ');
  const moved = state();
  assert.equal(moved.prepare('SELECT count(*) AS n FROM wiki_pages').get().n, 0);
  moved.close();
  // A file of the earlier form, with the page as Base64 at its end, is written in this one when
  // the page moves, also by a work that then fails.
  // A page of the form that cited a passage for each statement becomes its text without the
  // markers, written from the documents it cited.
  const { sources: kept, ...rest } = record(html);
  const passage = (line) => ({ source: kept[0].source, range: { startLine: line, endLine: line }, byteRange: { start: 0, end: 3 } });
  const legacy = { ...rest, id: 'concepts/legacy', title: '古い形のファイル', markdown: '古い形。 [1]\n\n同じ資料から。 [2] [3]',
    citations: [passage(1), passage(2), { source: kept[1].source }] };
  await writeFile(join(base, 'wiki', 'concepts', 'legacy.html'),
    '<!doctype html>\n<main><h1>古い形のファイル</h1></main>\n</html>\n<!-- knowledge-nest:publication ' + Buffer.from(JSON.stringify(legacy)).toString('base64url') + ' -->\n');
  const older = state();
  older.prepare('INSERT INTO wiki_pages VALUES (?, ?)').run(legacy.id, JSON.stringify(legacy));
  older.close();
  await assert.rejects(nest('--project', 'kb', 'ingest', join(root, 'missing.md'), '--json'));
  const converted = record(await readFile(join(base, 'wiki', 'concepts', 'legacy.html'), 'utf8'));
  assert.equal(converted.title, '古い形のファイル');
  assert.equal(converted.markdown, '古い形。\n\n同じ資料から。');
  assert.equal(converted.citations, undefined);
  assert.deepEqual(converted.sources.map(({ source }) => source.objectId), ['guide.md', 'daily.md']);
  const emptied = state();
  assert.equal(emptied.prepare('SELECT count(*) AS n FROM wiki_pages').get().n, 0);
  emptied.close();

  // Categories. The first ingest wrote the category the model gave into the rules, and a page
  // keeps its category when the model gives another.
  const rulesNow = await readFile(join(base, 'rules.md'), 'utf8');
  assert.match(rulesNow, /## 分類\n[\s\S]*\n- スクラム\n\n## ページ/u);
  assert.equal(record(await readFile(scrumFile, 'utf8')).category, 'スクラム');
  // A person adds a category; the check finds a page that fits it better, and the page moves.
  await writeFile(join(base, 'rules.md'), rulesNow.replace('\n- スクラム\n', '\n- スクラム\n- 資料\n'));
  const misplaced = (await run('lint', '--content')).data.issues.filter(({ code }) => code === 'CATEGORY_MISMATCH');
  assert.deepEqual(misplaced.map(({ page, category, suggested, description }) => [page, category, suggested, description]),
    [['sources/guide', 'スクラム', '資料', 'sources/guide：分類は「スクラム」ですが、「資料」のほうが合っていそうです']]);
  const recategorized = await run('instruct', 'sources/guide', '--category', '資料');
  assert.equal(recategorized.status, 'succeeded', JSON.stringify(recategorized.errors));
  assert.equal(record(await readFile(join(base, 'wiki', 'sources', 'guide.html'), 'utf8')).category, '資料');
  // A category that is not in the rules, and a page without one, are found without the model.
  await run('instruct', 'concepts/legacy', '--category', '未知');
  const checked = await run('lint');
  assert.deepEqual(checked.data.issues.filter(({ code }) => code.endsWith('CATEGORY')).map(({ code, page, description }) => [code, page, description]),
    [['UNKNOWN_CATEGORY', 'concepts/legacy', 'concepts/legacy：分類「未知」は決まり（rules.md）の分類にありません']]);
  // Moving a page back is undone like any work.
  await run('undo', recategorized.run_id);
  assert.equal(record(await readFile(join(base, 'wiki', 'sources', 'guide.html'), 'utf8')).category, 'スクラム');
  await run('instruct', 'sources/guide', '--category', '資料');

  // The contents name the kinds of pages and the three operations.
  const contents = await readFile(join(base, 'wiki', 'index.html'), 'utf8');
  // The pages by category, in the order of the rules, then the others; the files of raw/ with
  // what became of each.
  assert.match(contents, /<h2 class="root">wiki\/<small>AI が書くページ · 4 件<\/small><\/h2><section data-group><h3>スクラム<small>2<\/small><\/h3>.*<h3>資料<small>1<\/small><\/h3>.*<h3>未知<small>1<\/small><\/h3>/su);
  assert.match(contents, /<h2 class="root"><a href="\.\.\/raw\/">raw\/<\/a><small>集めた資料（原本）· 5 件<\/small><\/h2>/u);
  assert.match(contents, /<strong>other\.md<\/strong><small class="done">得るものなし<\/small>/u);
  for (const name of ['取り込む（ingest）', '質問する（query）', '点検する（lint）', '決まり（schema）', '記録（log）']) assert.ok(contents.includes(name), name);
});

test('rewrites the pages a source adds to, with a model that writes well', { timeout: 300_000 }, async (t) => {
  const root = await realpath(await mkdtemp(join(tmpdir(), 'knowledge-nest-wiki-rewrite-')));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const requests = [];
  const server = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const body = JSON.parse(Buffer.concat(chunks).toString('utf8'));
    requests.push(body);
    const input = JSON.parse(body.messages[1].content);
    const source = (word) => input.sources.find(({ text }) => text.includes(word));
    let result;
    if (body.format.properties.update) result = { update: ['concepts/scrum', 'no/such-page'] };
    else if (source('改訂')) {
      // The whole page, revised: a paragraph as it was, one written anew with the new source, one
      // left out, and one with a name and a year that no source has.
      result = { pages: [{ id: 'concepts/scrum', title: 'スクラム', category: 'スクラム', sources: [source('改訂').id],
        markdown: 'スクラムは枠組みである。\n\nスプリントは1か月以内とされるが、2週間が多い。\n\n「スクラム憲章」は2001年に書かれた。' }] };
    } else {
      const { id, text } = input.sources[0];
      result = { pages: [{ id: 'concepts/scrum', title: 'スクラム', category: 'スクラム', sources: [id], markdown: text.trim().split('\n').join('\n\n') }] };
    }
    response.setHeader('content-type', 'application/json');
    response.end(JSON.stringify({ done: true, message: { content: JSON.stringify(result) } }));
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  t.after(() => new Promise((resolve) => { server.closeAllConnections(); server.close(resolve); }));

  const env = { ...process.env, KNOWLEDGE_NEST_GLOBAL_CONFIG: join(root, 'global.yaml') };
  const nest = (...args) => runNode(process.execPath, [cli, ...args], { cwd: root, env, encoding: 'utf8', timeout: 60_000 });
  const run = async (...args) => JSON.parse((await nest('--project', 'kb', ...args, '--json')).stdout);
  await nest('init', 'wiki', 'kb', '--purpose', 'スクラムの要点');
  const configPath = join(root, 'kb', 'KnowledgeNestfile.yaml');
  const config = parse(await readFile(configPath, 'utf8'));
  config.models.writer.model = 'testing';
  config.models.writer.base_url = 'http://127.0.0.1:' + server.address().port;
  // The way of a model that writes well; with OpenAI or Strata it is the default.
  config.ingress.modules[2].with.update = 'rewrite';
  await writeFile(configPath, stringify(config));

  await writeFile(join(root, 'kb', 'raw', 'guide.md'), 'スクラムは枠組みである。\nスプリントは1か月以内。\nデイリースクラムは15分。\n');
  assert.equal((await run('ingest', join(root, 'kb', 'raw', 'guide.md'))).status, 'succeeded');
  await writeFile(join(root, 'kb', 'raw', 'length.md'), '改訂\nスプリントは2週間が多い。\n');
  const revised = await run('ingest', join(root, 'kb', 'raw', 'length.md'));
  assert.equal(revised.status, 'succeeded', JSON.stringify(revised.errors));
  // The model named the page from the list, then read it in full with the new source.
  assert.deepEqual(Object.keys(requests[1].format.properties), ['update']);
  assert.deepEqual(JSON.parse(requests[1].messages[1].content).pages, [{ id: 'concepts/scrum', title: 'スクラム', summary: 'スクラムは枠組みである。\n\nスプリントは1か月以内。\n\nデイリースクラムは15分。' }]);
  const writing = JSON.parse(requests[2].messages[1].content);
  assert.deepEqual(writing.pages_needing_revision, ['concepts/scrum']);
  assert.equal(writing.pages[0].markdown, 'スクラムは枠組みである。\n\nスプリントは1か月以内。\n\nデイリースクラムは15分。');
  assert.deepEqual(writing.sources.map(({ text }) => text), ['改訂\nスプリントは2週間が多い。\n']);
  assert.match(requests[2].messages[0].content, /return the whole page, revised with what the sources add or change/u);
  assert.match(requests[2].messages[0].content, /copy numbers, dates, names and quotations exactly/u);
  // The page is what the model returned, written from both sources. What it says that neither
  // source has is pointed out.
  assert.equal(revised.data.pages[0].current, 'スクラムは枠組みである。\n\nスプリントは1か月以内とされるが、2週間が多い。\n\n「スクラム憲章」は2001年に書かれた。');
  assert.deepEqual(revised.warnings.map(({ code, page, literals }) => [code, page, literals]), [['UNSUPPORTED_LITERAL', 'concepts/scrum', ['スクラム憲章', '2001']]]);
  const page = JSON.parse(/id="knowledge-nest-page">(.*?)<\/script>/su.exec(await readFile(join(root, 'kb', 'wiki', 'concepts', 'scrum.html'), 'utf8'))[1]);
  assert.deepEqual(page.sources.map(({ source }) => source.objectId), ['guide.md', 'length.md']);
  const lint = await run('lint');
  assert.deepEqual(lint.data.issues.filter(({ code }) => code === 'UNSUPPORTED_LITERAL').map(({ literal, description }) => [literal, description]), [
    ['スクラム憲章', 'concepts/scrum：「スクラム憲章」が出典の資料に見つかりません'],
    ['2001', 'concepts/scrum：「2001」が出典の資料に見つかりません'],
  ]);
  // Undo brings the earlier page back.
  await run('undo', revised.run_id);
  assert.match(await readFile(join(root, 'kb', 'wiki', 'concepts', 'scrum.html'), 'utf8'), /デイリースクラムは15分。/u);
});
