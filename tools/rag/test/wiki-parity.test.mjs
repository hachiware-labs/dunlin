// Spec 28.12 step 5a: the commands of Wiki and PARA projects run in Rust and do what the
// JavaScript implementation (test/fixtures/wiki-reference) did: the same model requests, outputs,
// errors, stored pages, sources, jobs, issues and work, and the same published HTML files. Two
// copies of each project run the same commands, one through the CLI and one through the reference.
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { cp, mkdir, mkdtemp, readdir, readFile, realpath, rm, writeFile } from 'node:fs/promises';
import { DatabaseSync } from 'node:sqlite';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { parse, stringify } from 'yaml';

import { KnowledgeNest } from 'knowledge-nest';
import { chunkCorpus } from './fixtures/cli-reference/project-content.mjs';
import { atomicWrite } from './fixtures/wiki-reference/files.mjs';
import { ensureWiki, runWiki, validateWiki } from './fixtures/wiki-reference/project-wiki.mjs';
import { trackWork } from './fixtures/wiki-reference/project-wiki-work.mjs';

const cli = fileURLToPath(new URL('../cli.mjs', import.meta.url));
const runNode = promisify(execFile);

const firstLine = (text) => text.split('\n').map((line) => line.trim()).find(Boolean) ?? '';
const slug = (text) => text.includes('ポンプ') ? 'pump' : text.includes('フィルター') ? 'filter'
  : text.includes('点検記録') ? 'records' : text.includes('aaaa') ? 'long' : 'other';

/** The answer of the test model, computed from the request alone. */
function answer(body) {
  const request = JSON.parse(body);
  const system = request.messages[0].content;
  const input = JSON.parse(request.messages[1].content);
  const properties = request.format.properties;
  if (JSON.stringify(input).includes('壊れ')) return 'not json';
  // A concept two pages hold, one no page holds, and one that has a page; a line of each kind.
  if (properties.concepts) {
    return { concepts: [{ name: '交換', pages: input.pages.map(({ item }) => item) }, { name: '存在しない概念', pages: [1] },
      { name: input.pages[0].id, pages: [1] }, { name: '交換', pages: [1] }], questions: [' 点検の周期は誰が決めるか ', ''], sources: ['メーカーの保守手順書'] };
  }
  if (properties.issues) {
    const [first, ...related] = input.items;
    return { issues: related.flatMap((next) => {
      if (first.type === 'source' && next.type === 'source') return [];
      const quote = (item) => item.text.split(/[\n[]/u)[0].trim().slice(0, 40);
      const refs = { first: { item: first.item, quote: quote(first) }, second: { item: next.item, quote: quote(next) } };
      return first.type !== next.type ? [{ ...refs, kind: 'source_disagreement', description: '資料との違い' }]
        : [{ ...refs, kind: 'duplicate', description: '同じ記述' }, { ...refs, kind: 'contradiction', description: 'ページ間の矛盾' },
          { ...refs, second: { ...refs.second, quote: '存在しない引用' }, kind: 'contradiction', description: '不正な引用' },
          { ...refs, first: { ...refs.first, item: 9 }, kind: 'duplicate', description: '範囲外の番号' }];
    }) };
  }
  if (properties.conflict) {
    const { instruction, page, sources, supplement } = input;
    const quote = firstLine(sources[0].text);
    if (instruction.includes('矛盾')) {
      return supplement ? { conflict: '', pages: [{ id: page.id, title: page.title, facts: [
        { text: '原本の記述。', citations: [{ id: 1, quote }] },
        { text: '補足では周期は二週間。<script>x</script>', citations: [{ id: sources.at(-1).id, quote: '周期は二週間。' }] }] }] }
        : { conflict: '原本と食い違います', pages: [] };
    }
    if (instruction.includes('分割')) {
      return { conflict: '', pages: [{ id: page.id, title: page.title, facts: [{ text: '前半。', citations: [{ id: 1, quote }] }] },
        { id: page.id + '-detail', title: page.title + 'の詳細', facts: [{ text: '後半。', citations: [{ id: 1, quote }] }] }] };
    }
    if (instruction.includes('重複')) {
      return { conflict: '', pages: [{ id: page.id, title: 't', facts: [{ text: 'a', citations: [{ id: 1, quote }] }] },
        { id: page.id.toUpperCase(), title: 't', facts: [{ text: 'b', citations: [{ id: 1, quote }] }] }] };
    }
    return { conflict: '', pages: [{ id: page.id, title: page.title + '（改訂）', facts: [{ text: '改訂: ' + quote, citations: [{ id: 1, quote }] }] }] };
  }
  if (properties.pages) {
    const prefix = system.includes('Projects/') ? 'Projects/' : '';
    if (input.sources.some(({ text }) => text.includes('引用なし'))) {
      return { pages: [{ id: 'bad', title: 'bad', facts: [{ text: 'x', citations: [{ id: 1, quote: '存在しない' }] }] }] };
    }
    const pages = new Map();
    for (const source of input.sources) {
      const id = input.pages_needing_revision[0] ?? prefix + slug(source.text);
      const quote = firstLine(source.text);
      const text = slug(source.text) === 'pump' ? quote + 'について。詳細は[フィルター](filter.html)と[なし](none.html)。'
        : slug(source.text) === 'long' ? 'b'.repeat(1199) + '😀 長い記述' : quote + 'について。';
      const page = pages.get(id) ?? { id, title: id + ' の説明', facts: [] };
      page.facts.push({ text, citations: [{ id: source.id, quote }] });
      pages.set(id, page);
    }
    return { pages: [...pages.values()] };
  }
  if (input.question.includes('範囲外')) return { facts: [] };
  const [page] = input.pages;
  const paragraph = page.markdown.split('\n\n')[0].replace(/(?: \[\d+\])+$/u, '');
  if (input.question.includes('不正')) return { facts: [{ text: '不正', citations: [{ id: 1, quote: '存在しない引用' }] }] };
  return { facts: [{ text: '回答です。', citations: [{ id: 1, quote: paragraph.slice(0, 20) }] }] };
}

async function model(t) {
  const bodies = [];
  const server = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const body = Buffer.concat(chunks).toString('utf8');
    bodies.push(body);
    const result = answer(body);
    response.setHeader('content-type', 'application/json');
    response.end(JSON.stringify({ done: true, message: { content: typeof result === 'string' ? result : JSON.stringify(result) } }));
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  t.after(() => new Promise((resolve) => { server.closeAllConnections(); server.close(resolve); }));
  return { url: 'http://127.0.0.1:' + server.address().port, bodies };
}

/** Replaces times, process IDs and SHA-256 digests by placeholders in the order they appear. */
function normalizer() {
  const digests = new Map();
  return (text) => text
    // The style of a page is the CLI's own, also in the files a work kept.
    .replace(/<style>[^<]*<\/style>/gu, '<style></style>')
    .replace(/<!-- knowledge-nest:publication ([\w-]+) -->/gu, (_, data) => '<!-- ' + Buffer.from(data, 'base64url').toString('utf8') + ' -->')
    .replace(/\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z/gu, 'TIME')
    // Also in the record of a page file that a work kept as text, escaped once more.
    // The copy of a source in raw/ keeps the time of its file, which each side writes to the
    // millisecond in its own way.
    .replace(/(\\*")(acquiredAtMs|modifiedAtMs|pid)\1:\d+/gu, '$1$2$1:0')
    .replace(/(?<![0-9a-f])[0-9a-f]{64}(?![0-9a-f])/gu, (digest) => {
      if (!digests.has(digest)) digests.set(digest, 'digest-' + digests.size);
      return digests.get(digest);
    });
}

/** Asserts that two texts are equal, showing where they first differ. */
function equalText(left, right, label) {
  let at = 0;
  while (at < left.length && left[at] === right[at]) at++;
  assert.equal(left.slice(Math.max(0, at - 400), at + 400), right.slice(Math.max(0, at - 400), at + 400), label);
  assert.equal(left.length, right.length, label);
}

// The pages are the files of the folder, compared below; the reference kept them in a table too.
const tables = ['wiki_sources', 'wiki_jobs', 'wiki_issues', 'wiki_work', 'wiki_reviewed_material', 'wiki_content_pairs', 'wiki_suggestions'];

/** The rows of the Wiki tables and the published files, with each publication's record decoded. */
async function snapshot(base) {
  const db = new DatabaseSync(join(base, '.knowledge-nest', 'state.sqlite'), { readOnly: true });
  try {
    const rows = Object.fromEntries(tables.map((table) => [table, db.prepare(`SELECT * FROM ${table} ORDER BY rowid`).all()
      .map((row) => ({ ...row }))]));
    const files = {};
    const walk = async (directory, prefix) => {
      for (const entry of (await readdir(directory, { withFileTypes: true })).sort((a, b) => a.name < b.name ? -1 : 1)) {
        if (entry.isDirectory()) await walk(join(directory, entry.name), prefix + entry.name + '/');
        // The contents and the log of the folder are the CLI's own.
        else if (!prefix && ['index.html', 'log.html'].includes(entry.name)) continue;
        else files[prefix + entry.name] = (await readFile(join(directory, entry.name), 'utf8')).replace(/<style>[\s\S]*?<\/style>/u, '<style></style>').replace(/<!-- knowledge-nest:publication ([\w-]+) -->/u,
          (_, data) => '<!-- ' + Buffer.from(data, 'base64url').toString('utf8') + ' -->');
      }
    };
    // A Wiki keeps its pages in `wiki`, PARA in `site`.
    await walk(join(base, (await readdir(base)).includes('wiki') ? 'wiki' : 'site'), '');
    return { rows, files, rules: await readFile(join(base, 'rules.md'), 'utf8') };
  } finally {
    db.close();
  }
}

/** A Wiki or PARA project and its copy, which run the same commands through the CLI and the reference. */
async function projects(t, root, type, url) {
  const env = { ...process.env, KNOWLEDGE_NEST_GLOBAL_CONFIG: join(root, 'global.yaml') };
  await runNode(process.execPath, [cli, 'init', type, type, '--purpose', '設備保全の手順'], { cwd: root, env });
  const base = await realpath(join(root, type));
  const configPath = join(base, 'KnowledgeNestfile.yaml');
  const config = parse(await readFile(configPath, 'utf8'));
  config.models.writer.model = 'testing';
  config.models.writer.base_url = url;
  config.models.broken = { provider: 'ollama', kind: 'chat', base_url: 'not a url', model: 'x' };
  config.ingress.modules[2].with.max_pages_per_job = 4;
  await writeFile(configPath, stringify(config));
  const refBase = join(root, type + '-ref');
  await cp(base, refBase, { recursive: true });
  const paths = (directory) => ({ data: join(directory, '.knowledge-nest'), state: join(directory, '.knowledge-nest', 'state.sqlite'),
    artifacts: join(directory, '.knowledge-nest', 'artifacts'), corpus: join(directory, '.knowledge-nest', 'corpus') });
  const cliRun = (args) => runNode(process.execPath, [cli, '--project', type, ...args, '--json'],
    { cwd: root, env, encoding: 'utf8', timeout: 60_000 })
    .then(({ stdout }) => JSON.parse(stdout), (error) => ({ exitCode: error.code, ...JSON.parse(error.stdout) }));
  const reference = async (command, positionals, values, runId) => {
    const configText = await readFile(join(refBase, 'KnowledgeNestfile.yaml'), 'utf8');
    const projectConfig = parse(configText);
    const { plan, serveSettings } = validateWiki(projectConfig, join(refBase, 'KnowledgeNestfile.yaml'), () => null);
    const project = { root: refBase, configRoot: refBase, config: projectConfig, paths: paths(refBase), wikiPlan: plan, serveSettings };
    const readOnly = Boolean(values['dry-run']) || ['cite', 'rules', 'progress'].includes(command) || command === 'verify' && !values.content;
    // A check changes nothing, but it is a work of the log like the others.
    const tracked = !readOnly || command === 'verify';
    const db = new DatabaseSync(project.paths.state, { readOnly: !tracked });
    db.exec('PRAGMA busy_timeout = 500');
    if (!readOnly) ensureWiki(db);
    const warnings = [];
    const input = values.persistent ? [null, ...positionals] : positionals;
    const execute = (progress) => runWiki(db, project, command, input, values, runId, project.paths, {
      atomicWrite, warnings, progress,
      open: (path, keyword) => KnowledgeNest.open(path, { corpusBackend: 'sqlite', keywordNgrams: true, keywordBigrams: keyword.short_query === 'bigram' }),
      chunk: (document) => chunkCorpus({ groups: [['docs', [document]]], unicode: 'nfkc' }, { size: 800, overlap: 100 })[0][0],
    });
    try {
      return { output: tracked ? await trackWork(db, runId, command, values.page, execute) : await execute(), warnings };
    } finally {
      db.close();
    }
  };
  return { base, refBase, cliRun, reference, configPath };
}

test('runs Wiki and PARA commands like the former JavaScript code', { timeout: 600_000 }, async (t) => {
  const root = await realpath(await mkdtemp(join(tmpdir(), 'knowledge-nest-wiki-parity-')));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const { url, bodies } = await model(t);
  const sources = join(root, 'sources');
  await mkdir(join(sources, 'sub'), { recursive: true });
  const files = {
    'manual.md': '# ポンプの保守\n\n交換手順を確認する。\n\n月に一度ポンプを点検する。\n',
    'filter.txt': 'フィルターは毎月交換する。\n清掃は毎週行う。\n',
    'long.txt': 'a'.repeat(4499) + '😀tail\nsecond line\n',
    'sub/records.md': '## 点検\n\n点検記録を保管する。\n',
    '.hidden.txt': '読まない\n',
    'image.xyz': 'unsupported\n',
  };
  for (const [name, content] of Object.entries(files)) await writeFile(join(sources, name), content);
  const [actualIds, referenceIds] = [normalizer(), normalizer()];
  // Each copy keeps the sources in its own raw/: the paths of the reference's copy are named as
  // those of the CLI's.
  const expectedIds = (text) => referenceIds(text.replace(/(?<=[\\/])(wiki|para)-ref(?=[\\/])/gu, '$1'));

  const check = (pair) => {
    /**
     * Runs a command on both copies and compares requests, outputs, warnings and errors. `message`
     * replaces the expected error message where Rust deliberately differs from the reference.
     */
    return async (command, positionals = [], values = {}, message = undefined) => {
      const label = JSON.stringify([command, positionals, values]);
      const args = [command, ...Object.entries(values).flatMap(([key, value]) => value === true ? ['--' + key] : ['--' + key, value]), ...positionals];
      const first = bodies.length;
      const actual = await pair.cliRun(args);
      const sent = bodies.splice(first);
      let expected;
      let failure;
      try {
        expected = await pair.reference(command, positionals, values, actual.run_id ?? undefined);
      } catch (error) {
        failure = error;
      }
      const received = bodies.splice(first);
      equalText(actualIds(JSON.stringify(sent)), expectedIds(JSON.stringify(received)), label + ' requests');
      if (failure) {
        // The code and exit code the CLI gives the error.
        const argument = failure.code === 'INVALID_ARGUMENT';
        assert.equal(actual.status, 'failed', label + ': ' + failure.message);
        assert.equal(actual.errors[0].message, message ?? failure.message, label);
        assert.equal(actual.errors[0].code, failure.exitCode ? failure.code : argument ? 'INVALID_ARGUMENT' : 'EXECUTION_FAILED', label);
        assert.equal(actual.exitCode, failure.exitCode ?? (argument ? 2 : 1), label);
        return actual;
      }
      assert.equal(actual.status, 'succeeded', label + ' ' + JSON.stringify(actual.errors));
      equalText(actualIds(JSON.stringify(actual.data)), expectedIds(JSON.stringify(expected.output)), label);
      assert.equal(actualIds(JSON.stringify(actual.warnings)), expectedIds(JSON.stringify(expected.warnings)), label + ' warnings');
      return { ...actual, sent };
    };
  };
  const compareState = async (pair, label) => {
    const [actual, expected] = [await snapshot(pair.base), await snapshot(pair.refBase)];
    for (const table of tables) {
      // The recorded error of an ingress whose quotes all fail differs on purpose (see `bad.txt`).
      const right = expectedIds(JSON.stringify(expected.rows[table])).replaceAll(
        'Every citation must identify a unique verbatim quote in the supplied evidence.',
        'None of the 1 statements the model wrote quoted the sources exactly, so no page was saved. Run ingress again or use a stronger model.');
      const left = actualIds(JSON.stringify(actual.rows[table]));
      let at = 0;
      while (at < left.length && left[at] === right[at]) at++;
      assert.equal(left.slice(Math.max(0, at - 300), at + 300), right.slice(Math.max(0, at - 300), at + 300), label + ' ' + table);
    }
    assert.deepEqual(Object.keys(actual.files), Object.keys(expected.files), label + ' files');
    for (const name of Object.keys(actual.files)) {
      assert.equal(actualIds(actual.files[name]), expectedIds(expected.files[name]), label + ' ' + name);
    }
    assert.equal(actual.rules, expected.rules, label + ' rules');
  };
  const configure = async (pair, change) => {
    for (const path of [pair.configPath, join(pair.refBase, 'KnowledgeNestfile.yaml')]) {
      const config = parse(await readFile(path, 'utf8'));
      change(config);
      await writeFile(path, stringify(config));
    }
  };

  const wiki = await projects(t, root, 'wiki', url);
  const same = check(wiki);
  await same('ingress', [sources], { 'dry-run': true });
  await same('ingress', [sources]);
  await configure(wiki, (config) => { config.ingress.modules[0].with.unsupported = 'skip'; });
  await configure(wiki, (config) => { config.ingress.modules[2].with.max_pages_per_job = 1; });
  await same('ingress', [sources]);
  await configure(wiki, (config) => { config.ingress.modules[2].with.max_pages_per_job = 4; });
  await same('ingress', [join(root, 'missing-file')]);
  const ingested = await same('ingress', [sources]);
  assert.equal(ingested.status, 'succeeded', JSON.stringify(ingested.errors));
  assert.deepEqual(ingested.data.pages.map(({ id }) => id).sort(), ['filter', 'long', 'pump', 'records']);
  await same('ingress', [join(sources, 'sub')], { 'source-only': true });
  await same('ingress', [], { pending: true, 'dry-run': true });
  await same('ingress', [], { pending: true });
  await same('query', ['ポンプの交換手順']);
  const asked = await same('query', ['フィルターの交換'], { target: 'section' });
  await same('query', ['範囲外の質問']);
  await same('query', ['存在しない語句です']);
  await same('query', ['不正な引用']);
  await same('query', ['壊れた応答']);
  await same('query', ['交換'], { page: 'filter' });
  await same('cite', ['pump', '1']);
  await same('cite', ['pump', '9']);
  await same('cite', ['missing', '1']);
  const saved = await same('save-answer', [asked.run_id, 'faq', '交換の FAQ']);
  await same('save-answer', [asked.run_id, 'faq', '重複']);
  await same('save-answer', ['run-missing', 'x', 'y']);
  await same('save-answer', [asked.run_id, 'bad:id', 'x']);
  await same('undo', [saved.run_id]);
  await same('undo', ['run-missing']);
  await same('instruct', ['pump', '短くしてください']);
  await same('instruct', ['pump', '矛盾する周期に変更']);
  await same('instruct', ['pump', '矛盾する周期に変更'], { supplement: '周期は二週間。', by: '点検担当' });
  await same('instruct', ['pump', '矛盾'], { supplement: '周期は二週間。' });
  await same('instruct', ['pump', '分割してください']);
  await same('instruct', ['pump', '重複した ID']);
  await same('instruct', ['missing', '指示']);
  const rules = await same('instruct', ['手順は番号付きで書く'], { persistent: true });
  await same('rules');
  await same('undo', [rules.run_id]);
  await same('verify');
  const issues = await same('verify', [], { 'all-issues': true });
  const orphan = issues.data.issues.find(({ code }) => code === 'ORPHAN_PAGE');
  await same('issue', [orphan.id, 'dismissed']);
  await same('issue', ['issue-bad', 'dismissed']);
  await same('verify', [], { page: 'pump' });
  await same('archive', ['filter']);
  await same('query', ['フィルター']);
  await same('archive', ['filter'], { restore: true });
  await same('archive', ['missing']);
  await compareState(wiki, 'wiki');
  await writeFile(join(sources, 'filter.txt'), 'フィルターは隔週で交換する。\n清掃は毎週行う。\n');
  const updated = await same('ingress', [join(sources, 'filter.txt')]);
  assert.deepEqual(updated.data.pages.map(({ id }) => id), ['filter']);
  await same('verify');
  const inspected = await same('verify', [], { content: true });
  assert.deepEqual(new Set(inspected.data.issues.filter(({ kind }) => kind).map(({ kind }) => kind)), new Set(['contradiction', 'duplicate', 'source_disagreement']));
  // The source excerpts are cut at 4500 units, inside the emoji of long.txt.
  assert.ok(inspected.sent.some((body) => body.includes(String.raw`a\\ud83d`)), 'a lone surrogate at the source excerpt end');
  await same('verify', [], { content: true });
  await same('verify', [], { content: true, page: 'pump' });
  await same('verify', [], { content: true, 'all-issues': true });
  const work = await same('instruct', ['pump', '短くしてください']);
  await same('progress', [work.run_id]);
  await same('progress', ['run-missing']);
  await writeFile(join(sources, 'bad.txt'), '引用なしの資料\n');
  // The reference fails on the first unverified quote. Rust saves the statements that quote their
  // sources and fails only when none does (spec 16).
  await same('ingress', [join(sources, 'bad.txt')], {}, 'None of the 1 statements the model wrote quoted the sources exactly, so no page was saved. Run ingress again or use a stronger model.');
  await same('ingress', [join(sources, 'bad.txt')], { 'source-only': true });
  await compareState(wiki, 'wiki after updates');

  const para = await projects(t, root, 'para', url);
  const paraSame = check(para);
  await paraSame('ingress', [join(sources, 'manual.md')]);
  const archived = await paraSame('archive', ['Projects/pump']);
  await compareState(para, 'para archived');
  await paraSame('undo', [archived.run_id]);
  const again = await paraSame('archive', ['Projects/pump']);
  await paraSame('archive', ['Projects/pump']);
  await paraSame('undo', [again.run_id]);
  await paraSame('archive', ['Projects/pump'], { restore: true });
  await paraSame('save-answer', ['run-missing', 'Projects/x', 'x']);
  await compareState(para, 'para');
});
