// PARA as Tiago Forte keeps it: people list their projects and areas in the rules, the AI writes
// a note for each source of use and puts it where it will be used, the source is kept beside its
// note, and a project that is done goes to Archives with all its notes.
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { mkdir, mkdtemp, readFile, realpath, rm, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { parse, stringify } from 'yaml';

const cli = fileURLToPath(new URL('../cli.mjs', import.meta.url));
const runNode = promisify(execFile);

/** The notes of the test model: each source by its first line, where it will be used. */
function answer(request) {
  const input = JSON.parse(request.messages[1].content);
  if (request.format.properties.issues) return { issues: [] };
  if (request.format.properties.concepts) return { concepts: [], questions: [], sources: [], misplaced: [] };
  const notes = [];
  for (const { id, text } of input.sources) {
    const [first, second] = text.split('\n');
    const note = (place, links = '') => notes.push({ id: place, title: first, category: '', sources: [id],
      markdown: `${first}の資料。\n\n**要点**\n\n- ${second}\n\n**使い道**\n\n作業の前に読む。${links}` });
    // The place alone, as a small model gives it: the note is named after its source.
    if (first === 'ポンプ交換手順') note('Projects/ポンプ更新工事');
    if (first === '部品カタログ') note('Resources/部品/catalog', '\n\n交換の手順は[交換手順](/Projects/ポンプ更新工事/pump)にある。');
    // A project the rules do not list.
    if (first === '架空') note('Projects/架空工事');
    // A note that exists, given for another source as a small model does.
    if (first === 'シール') {
      note('Resources/部品/catalog');
      // A second place for the same source.
      note('Areas/設備の定期保全');
    }
  }
  return { pages: notes };
}

test('places a note for each source where it will be used, and archives a project as a whole', { timeout: 300_000 }, async (t) => {
  const root = await realpath(await mkdtemp(join(tmpdir(), 'knowledge-nest-para-')));
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
  const run = async (...args) => JSON.parse((await nest('--project', 'p', ...args, '--json')).stdout);
  await nest('init', 'para', 'p', '--purpose', '設備保全の仕事');
  const base = join(root, 'p');
  const site = join(base, 'site');
  const configPath = join(base, 'KnowledgeNestfile.yaml');
  const config = parse(await readFile(configPath, 'utf8'));
  config.models.writer.model = 'testing';
  config.models.writer.base_url = 'http://127.0.0.1:' + server.address().port;
  await writeFile(configPath, stringify(config));
  const record = async (...path) => JSON.parse(/id="knowledge-nest-page">(.*?)<\/script>/su.exec(await readFile(join(site, ...path), 'utf8'))[1]);
  const exists = (path) => stat(path).then(() => true, () => false);

  // The rules of a new PARA: the projects and areas people list, the topics the AI adds, and how
  // notes are written and placed.
  const rules = await readFile(join(base, 'rules.md'), 'utf8');
  for (const part of ['## Projects', '## Areas', '## Resources', '## Archives', '## ノートの書き方', '## 置き場所の決め方', '資料/', '得るものなし']) {
    assert.ok(rules.includes(part), part);
  }
  // A person lists a project, with a deadline that has passed, and an area.
  await writeFile(join(base, 'rules.md'), rules
    .replace('AI はここにない取り組みを作らない。\n', 'AI はここにない取り組みを作らない。\n\n- ポンプ更新工事：2020-01-01 までに主ポンプを交換する\n')
    .replace('AI はここにない領域を作らない。\n', 'AI はここにない領域を作らない。\n\n- 設備の定期保全：停止を減らす\n'));

  await mkdir(join(root, 'inbox'));
  await writeFile(join(root, 'inbox', 'pump.md'), 'ポンプ交換手順\n交換は2時間かかる。\n');
  await writeFile(join(root, 'inbox', 'catalog.md'), '部品カタログ\nシールの型番はMS-200。\n');
  await writeFile(join(root, 'inbox', 'misc.md'), '天気\n晴れ。\n');
  await writeFile(join(root, 'inbox', 'wrong.md'), '架空\nどこにもない工事。\n');
  const ingested = await run('ingest', join(root, 'inbox'));
  assert.equal(ingested.status, 'succeeded', JSON.stringify(ingested.errors));
  assert.equal(requests.length, 1);
  assert.match(requests[0].messages[0].content, /one of places, or Resources\/<a new topic> when none fits/u);
  assert.deepEqual(ingested.data.pages.map(({ id }) => id).sort(), ['Projects/ポンプ更新工事/pump', 'Resources/部品/catalog']);
  // A note in a project the rules do not list is not saved, and its source waits to be worked in.
  assert.deepEqual(ingested.warnings.map(({ code, page }) => [code, page]), [['PLACE_INVALID', 'Projects/架空工事/wrong']]);
  assert.equal((await run('ingest', '--pending', '--dry-run')).data.pending, 1);
  // A source of no use is recorded as such.
  assert.deepEqual(ingested.data.no_material, ['misc.md']);
  // The copy of the source is beside its note, and the note links to it.
  assert.equal(await readFile(join(site, 'Projects', 'ポンプ更新工事', '資料', 'pump.md'), 'utf8'), 'ポンプ交換手順\n交換は2時間かかる。\n');
  const pump = await record('Projects', 'ポンプ更新工事', 'pump.html');
  assert.match(decodeURIComponent(pump.sources[0].source.url), /\/site\/Projects\/ポンプ更新工事\/資料\/pump\.md$/u);
  assert.match(await readFile(join(site, 'Projects', 'ポンプ更新工事', 'pump.html'), 'utf8'), /<li id="source-1"><a href="%E8%B3%87%E6%96%99\/pump\.md">pump\.md<\/a>/u);
  // A topic of Resources the AI used is added to the rules.
  const listed = await readFile(join(base, 'rules.md'), 'utf8');
  assert.match(listed, /## Resources\n[\s\S]*\n- 部品\n\n## Archives/u);

  // Another source makes a note of its own: the model is not shown the other notes, and a note of
  // another source it names is taken as the place.
  await writeFile(join(root, 'seal.md'), 'シール\nシールは2年で交換する。\n');
  const seal = await run('ingest', join(root, 'seal.md'));
  assert.equal(seal.status, 'succeeded', JSON.stringify(seal.errors));
  const sent = JSON.parse(requests.at(-1).messages[1].content);
  assert.deepEqual(sent.pages, []);
  // The model is given the places a note can go to.
  assert.deepEqual(sent.places, ['Projects/ポンプ更新工事', 'Areas/設備の定期保全', 'Resources/部品']);
  // A source has one place: the first note for it is kept.
  assert.deepEqual(seal.data.pages.map(({ id }) => id), ['Resources/部品/seal']);
  assert.deepEqual(seal.warnings.map(({ code, page }) => [code, page]), [['NOTE_SKIPPED', 'Areas/設備の定期保全/seal']]);
  assert.doesNotMatch((await record('Resources', '部品', 'catalog.html')).markdown, /シール は|2年/u);

  // The check: a project past its deadline.
  const lint = await run('lint');
  assert.deepEqual(lint.data.issues.filter(({ code }) => code === 'PROJECT_OVERDUE').map(({ project, description }) => [project, description]),
    [['ポンプ更新工事', 'Projects/ポンプ更新工事：期限（2020-01-01）を過ぎています。終わっていれば nest archive Projects/ポンプ更新工事 で Archives へ移します']]);

  // The project is done: its notes, its folder and its line of the rules go to Archives, and the
  // links to its notes follow them.
  const archived = await run('archive', 'Projects/ポンプ更新工事');
  assert.equal(archived.status, 'succeeded', JSON.stringify(archived.errors));
  assert.equal(await exists(join(site, 'Projects', 'ポンプ更新工事')), false);
  assert.equal(await readFile(join(site, 'Archives', 'ポンプ更新工事', '資料', 'pump.md'), 'utf8'), 'ポンプ交換手順\n交換は2時間かかる。\n');
  const moved = await record('Archives', 'ポンプ更新工事', 'pump.html');
  assert.equal(moved.id, 'Archives/ポンプ更新工事/pump');
  assert.equal(moved.archived, true);
  assert.equal(moved.archived_from, 'Projects/ポンプ更新工事');
  assert.match(decodeURIComponent(moved.sources[0].source.url), /\/site\/Archives\/ポンプ更新工事\/資料\/pump\.md$/u);
  assert.match((await record('Resources', '部品', 'catalog.html')).markdown, /\[交換手順\]\(\/Archives\/ポンプ更新工事\/pump\)/u);
  const after = await readFile(join(base, 'rules.md'), 'utf8');
  assert.doesNotMatch(after.slice(after.indexOf('## Projects'), after.indexOf('## Areas')), /\n- ポンプ更新工事/u);
  assert.match(after, /## Archives\n[\s\S]*\n- ポンプ更新工事：2020-01-01 までに主ポンプを交換する（Projects から \d{4}-\d\d-\d\d に移した）\n/u);
  assert.ok((await run('lint')).data.issues.every(({ code }) => code !== 'PROJECT_OVERDUE'));

  // Undo puts everything back.
  await run('undo', archived.run_id);
  assert.equal((await record('Projects', 'ポンプ更新工事', 'pump.html')).archived, false);
  assert.equal(await exists(join(site, 'Archives', 'ポンプ更新工事')), false);
  assert.equal(await exists(join(site, 'Projects', 'ポンプ更新工事', '資料', 'pump.md')), true);
  assert.equal(await readFile(join(base, 'rules.md'), 'utf8'), listed);
  assert.match((await record('Resources', '部品', 'catalog.html')).markdown, /\(\/Projects\/ポンプ更新工事\/pump\)/u);

  // Archived again and then restored, the project is back in Projects with its line of the rules.
  await run('archive', 'Projects/ポンプ更新工事');
  const restored = await run('archive', '--restore', 'Archives/ポンプ更新工事');
  assert.equal(restored.status, 'succeeded', JSON.stringify(restored.errors));
  const back = await record('Projects', 'ポンプ更新工事', 'pump.html');
  assert.equal(back.archived, false);
  assert.equal(back.archived_from, undefined);
  const again = await readFile(join(base, 'rules.md'), 'utf8');
  assert.match(again.slice(again.indexOf('## Projects'), again.indexOf('## Areas')), /\n- ポンプ更新工事：2020-01-01 までに主ポンプを交換する\n/u);
  assert.doesNotMatch(again.slice(again.indexOf('## Archives')), /\n- ポンプ更新工事/u);
});
