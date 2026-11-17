import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { once } from 'node:events';
import { setTimeout as delay } from 'node:timers/promises';
import { mkdtemp, readFile, rename, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { promisify } from 'node:util';
import { execFile, spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { parse, stringify } from 'yaml';

import { Dunlin } from 'dunlin';

const runNode = promisify(execFile);
const cli = fileURLToPath(new URL('../cli.mjs', import.meta.url));

test('uses trial settings without changing the active build and evaluates saved evidence', { timeout: 30_000 }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-evaluation-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const run = (...args) => runNode(process.execPath, [cli, ...args], { cwd: root, env: { ...process.env, DUNLIN_GLOBAL_CONFIG: join(root, 'global.yaml') }, encoding: 'utf8', timeout: 10_000 });
  const json = async (...args) => JSON.parse((await run('--project', 'kb', ...args, '--json')).stdout);
  await run('init', 'mcp', 'kb');
  await writeFile(join(root, 'kb', 'docs', 'guide.txt'), '交換手順の根拠。\n');
  const preview = await json('build', '--dry-run');
  assert.equal(preview.run_id, null);
  assert.equal((await json('status')).data.build_id, null);
  const baseline = (await json('build')).data;
  await json('eval-data', 'add', '--question', '手順', '--doc', 'docs:guide.txt', '--quote', '交換手順の根拠。');
  const original = (await json('eval', '--tag', 'baseline')).data;
  assert.equal(original.scores['evidence_recall@context'], 1);
  assert.equal((await json('status')).data.evaluation_status, 'evaluated');
  const config = parse(await readFile(join(root, 'kb', 'Dunlinfile.yaml'), 'utf8'));
  config.source.modules[0].with.path = './kb/docs';
  config.search.modules.at(-1).with.budget_chars = 10;
  await writeFile(join(root, 'trial.yaml'), stringify(config));
  const trial = (await json('build', '--from', 'transform', '--config', 'trial.yaml', '--tag', 'small')).data;
  assert.equal(trial.build_id, baseline.build_id);
  assert.equal(trial.serving_changed, false);
  assert.equal((await json('search', '手順', '--build', 'small')).data.context.text, '');
  assert.ok((await json('search', '手順')).data.context.text.length > 10);
  const scored = (await json('eval', '--build', 'small', '--tag', 'trial')).data;
  assert.equal(scored.scores['evidence_recall@context'], 0);
  const comparison = (await json('report', 'compare', 'baseline', 'trial')).data;
  assert.equal(comparison.comparable, false);
  assert.equal(comparison.common_case_scores['evidence_recall@context'].delta, -1);
  assert.deepEqual(comparison.differences.budget_chars, [4000, 10]);
  const listed = (await json('report', 'list')).data[0];
  assert.ok(listed.columns.includes('budget_chars'));
  assert.equal(listed.results.length, 2);
  await json('tag', 'selected', 'small');
  assert.equal((await json('search', '手順', '--build', 'selected')).data.context.text, '');
  await json('use', 'small');
  assert.equal((await json('search', '手順')).data.context.text, '');
  assert.equal((await json('build')).data.documents, 1);
  await writeFile(join(root, 'kb', 'docs', 'guide.txt'), '新しい交換手順の記載。\n');
  assert.equal((await json('build', '--dry-run')).data.files[0].change, 'updated');
  await json('build');
  assert.equal((await json('status')).data.evaluation_status, 'stale');
  const sync = (await json('eval-data', 'sync', original.dataset_id)).data;
  assert.equal(sync.needs_review, 1);
  const difference = (await json('eval-data', 'diff', original.dataset_id, sync.id)).data;
  assert.equal(difference.changed[0].after.state, 'needs_review');
});

test('builds exact vector and hybrid search using the configured chunk ranges', { timeout: 30_000 }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-vector-next-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const requests = [];
  const server = createServer(async (request, response) => {
    const body = [];
    for await (const chunk of request) body.push(chunk);
    const { input } = JSON.parse(Buffer.concat(body));
    requests.push(input);
    response.setHeader('content-type', 'application/json');
    response.end(JSON.stringify({ embeddings: input.map((text) => /交換|取り替え/u.test(text) ? [1, 0] : [0, 1]) }));
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  t.after(() => new Promise((resolve) => server.close(resolve)));
  const run = (...args) => runNode(process.execPath, [cli, ...args], { cwd: root, env: { ...process.env, DUNLIN_GLOBAL_CONFIG: join(root, 'global.yaml') }, encoding: 'utf8', timeout: 10_000 });
  const json = async (...args) => JSON.parse((await run('--project', 'kb', ...args, '--json')).stdout);
  await run('init', 'mcp', 'kb');
  await writeFile(join(root, 'kb', 'docs', 'guide.txt'), 'フィルターを交換します。\n');
  await writeFile(join(root, 'kb', 'docs', 'safety.txt'), '絶縁手袋を使います。\n');
  const path = join(root, 'kb', 'Dunlinfile.yaml');
  const config = parse(await readFile(path, 'utf8'));
  config.models = { local: { provider: 'ollama', kind: 'embedding', model: 'testing', base_url: 'http://127.0.0.1:' + server.address().port } };
  config.index.modules.push({ use: 'index.vector', with: { model: 'local' } });
  config.search.modules.splice(2, 0, { use: 'retrieval.vector', with: { model: 'local', top_k: 10 } },
    { use: 'fusion.rrf', with: { k: 60 } });
  await writeFile(path, stringify(config));
  await json('validate');
  await json('build');
  assert.equal(requests.flat().length, 2);
  const result = (await json('search', '取り替え')).data;
  assert.equal(result.results[0].evidence.source.objectId, 'guide.txt');
  assert.ok(result.results[0].evidence.byteRange.end > 0);
  config.index.modules[0].with = { tokenizer: 'word', short_query: 'none' };
  await writeFile(path, stringify(config));
  await json('build', '--from', 'index');
  assert.equal(requests.flat().length, 3);
  config.transform.modules[1] = { use: 'chunk.semantic', with: { model: 'local', max_chars: 30, sentence_chars: 8, similarity: 0.55 } };
  await writeFile(path, stringify(config));
  const semantic = (await json('build', '--from', 'transform')).data;
  const calls = requests.length;
  const repeated = (await json('build', '--from', 'transform')).data;
  assert.equal(repeated.build_id, semantic.build_id);
  assert.equal(repeated.stages.transform.reused, true);
  assert.equal(requests.length, calls);
  await json('tag', 'exact', semantic.build_id);
  const tool = (await json('add', 'hannoy')).data;
  assert.deepEqual(tool.modules, ['hannoy.index', 'hannoy.search']);
  assert.ok((await json('modules', 'list')).data.some(({ name }) => name === 'hannoy.index'));
  const installed = parse(await readFile(path, 'utf8'));
  installed.index.modules[1] = { ...tool.configuration.index, with: { model: 'local', ef_construction: 100 } };
  installed.search.modules[2] = { ...tool.configuration.search, with: { model: 'local', top_k: 10, ef_search: 100 } };
  await writeFile(path, stringify(installed));
  await json('validate');
  const ann = (await json('build', '--from', 'index')).data;
  assert.equal(ann.vector.backend, 'hannoy');
  assert.equal(requests.length, calls);
  assert.equal((await json('search', '取り替え')).data.results[0].evidence.source.objectId, 'guide.txt');
  const toolDirectory = join(root, 'kb', '.dunlin', 'tools', 'hannoy', tool.version, process.platform + '-' + process.arch);
  await rename(toolDirectory, toolDirectory + '-missing');
  await assert.rejects(json('validate'), (error) => error.code === 2 && JSON.parse(error.stdout).errors[0].code === 'REQUIREMENT_MISSING');
  await rename(toolDirectory + '-missing', toolDirectory);
  await rm(join(root, 'kb', 'docs', 'safety.txt'));
  const delta = (await json('build')).data;
  assert.equal(delta.vector.indexed, 0);
  assert.ok(delta.vector.removed > 0);
  const old = (await json('search', '手袋', '--build', 'exact')).data;
  assert.ok(old.results.some(({ evidence }) => evidence.source.objectId === 'safety.txt'));
  assert.ok(!(await json('search', '手袋')).data.results.some(({ evidence }) => evidence.source.objectId === 'safety.txt'));
  await writeFile(join(root, 'kb', 'docs', 'procedures.txt'), '交換部品を準備します。\n');
  const addition = (await json('build')).data;
  assert.ok(addition.vector.indexed > 0);
  assert.ok(addition.vector.reused > 0);
  await assert.rejects(json('remove', 'hannoy'), { code: 2 });
  await json('use', 'exact');
  await json('remove', 'hannoy');
  assert.ok(!(await json('modules', 'list')).data.some(({ name }) => name === 'hannoy.index'));
});

test('ranks all rows when positions are omitted', async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-vector-rank-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const bytes = Buffer.alloc(3 * 8);
  for (const i of [0, 2]) bytes.writeFloatLE(1, i * 8);
  const file = join(root, 'vectors.f32');
  await writeFile(file, bytes);
  const options = { needle: [1, 0], rows: 3, limit: 3 };
  const explicit = await Dunlin.rankVectors(file, { ...options, positions: [2, 1, 0] });
  assert.deepEqual(await Dunlin.rankVectors(file, options), explicit);
  assert.deepEqual(explicit.map(({ i }) => i), [0, 2, 1]);
  assert.deepEqual(await Dunlin.rankVectors(file, { ...options, positions: [] }), []);
});

test('enforces trial counters and time without blocking short operations', { timeout: 35_000 }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-trials-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const calls = [];
  const server = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    calls.push(Buffer.concat(chunks).length);
    await delay(1500);
    if (!response.destroyed) response.end(JSON.stringify({ embeddings: [[1, 0]] }));
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  t.after(() => { server.closeAllConnections(); return new Promise((resolve) => server.close(resolve)); });
  const run = (...args) => runNode(process.execPath, [cli, ...args], { cwd: root, env: { ...process.env, DUNLIN_GLOBAL_CONFIG: join(root, 'global.yaml') }, encoding: 'utf8', timeout: 10_000 });
  const json = async (...args) => JSON.parse((await run('--project', 'kb', ...args, '--json')).stdout);
  const failure = async (code, ...args) => {
    try { await json(...args); assert.fail('Expected failure.'); }
    catch (error) { assert.equal(error.code, code); return JSON.parse(error.stdout); }
  };
  await run('init', 'mcp', 'kb');
  const configPath = join(root, 'kb', 'Dunlinfile.yaml');
  await writeFile(join(root, 'kb', 'docs', 'guide.txt'), '交換手順を確認する。\n');
  const base = (await json('build')).data;
  const config = parse(await readFile(configPath, 'utf8'));
  config.trial_limits = { max_runs: 1 };
  await writeFile(configPath, stringify(config));
  await json('build', '--tag', 'first');
  const candidatePath = join(root, 'kb', 'candidate.yaml');
  await writeFile(candidatePath, stringify({ ...config, trial_limits: { max_runs: 100 } }));
  const limited = await failure(4, 'build', '--config', candidatePath, '--tag', 'blocked');
  assert.equal(limited.errors[0].code, 'TRIAL_LIMIT_REACHED');
  assert.deepEqual(limited.data, { limit: 'max_runs', maximum: 1, used: 1 });
  await json('build', '--dry-run', '--tag', 'preview');
  assert.equal((await json('status')).data.trials.usage.runs, 1);
  const candidate = { ...config, models: { local: { provider: 'ollama', kind: 'embedding', model: 'test',
    base_url: 'http://127.0.0.1:' + server.address().port } }, trial_limits: undefined };
  candidate.index = { modules: [...config.index.modules, { use: 'index.vector', with: { model: 'local' } }] };
  await writeFile(candidatePath, stringify(candidate));
  config.trial_limits = { max_api_requests: 0 };
  await writeFile(configPath, stringify(config));
  assert.equal((await failure(4, 'build', '--from', 'index', '--config', candidatePath)).data.limit, 'max_api_requests');
  config.trial_limits = { max_api_input_bytes: 1 };
  await writeFile(configPath, stringify(config));
  assert.equal((await failure(4, 'build', '--from', 'index', '--config', candidatePath)).data.limit, 'max_api_input_bytes');
  // chunk.semantic calls the model from Rust, which asks JavaScript to count each request first.
  const semanticPath = join(root, 'kb', 'semantic.yaml');
  await writeFile(semanticPath, stringify({ ...candidate, transform: { modules: [config.transform.modules[0],
    { use: 'chunk.semantic', with: { model: 'local', max_chars: 100, sentence_chars: 50, similarity: 0.5 } }] } }));
  for (const [limit, maximum] of [['max_api_requests', 0], ['max_api_input_bytes', 1]]) {
    config.trial_limits = { [limit]: maximum };
    await writeFile(configPath, stringify(config));
    const refused = await failure(4, 'build', '--from', 'transform', '--config', semanticPath);
    assert.equal(refused.errors[0].code, 'TRIAL_LIMIT_REACHED');
    assert.equal(refused.data.limit, limit);
  }
  assert.equal(calls.length, 0);
  const used = (await json('status')).data.trials.usage.elapsed_ms;
  config.trial_limits = { max_elapsed_ms: used + 1300 };
  await writeFile(configPath, stringify(config));
  const slow = failure(4, 'build', '--from', 'index', '--config', candidatePath, '--tag', 'timed');
  for (let i = 0; !calls.length && i < 50; i++) await delay(20);
  assert.equal(calls.length, 1);
  const conflict = await failure(4, 'build');
  assert.equal(conflict.errors[0].code, 'BUILD_BUSY');
  await json('tag', 'concurrent', base.build_id);
  await json('eval-data', 'add', '--question', '手順', '--doc', 'docs:guide.txt', '--quote', '交換手順を確認する。');
  assert.equal((await slow).data.limit, 'max_elapsed_ms');
  const status = (await json('status')).data;
  assert.equal(status.build_id, base.build_id);
  assert.equal(status.trials.usage.api_requests, 1);
  assert.equal(status.trials.usage.api_input_bytes, calls[0]);
  await failure(3, 'search', '手順', '--build', 'timed');
});

test('writes and verifies a Wiki page with an explicitly configured model', { timeout: 30_000 }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-wiki-next-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const calls = [];
  let pauseModel;
  const server = createServer(async (request, response) => {
    const buffers = [];
    for await (const chunk of request) buffers.push(chunk);
    const input = JSON.parse(Buffer.concat(buffers));
    calls.push(input);
    if (pauseModel) await pauseModel.promise;
    const fact = { text: '交換手順を確認する。', citations: [{ id: 1, quote: '交換手順を確認する。' }] };
    const id = input.messages[0].content.includes('Projects/') ? 'Projects/maintenance' : 'maintenance';
    let result = input.format.properties.pages ? { pages: [{ id, title: '交換手順', facts: [fact] }],
      ...(input.format.properties.conflict && { conflict: '' }) } : { facts: [fact] };
    if (input.format.properties.conflict) {
      const { supplement, sources, instruction } = JSON.parse(input.messages[1].content);
      if (instruction === '交換周期を変更') {
        result = supplement ? { conflict: '', pages: [{ id, title: '交換手順', facts: [
          fact, { text: '担当者の補足では交換周期は3か月。<script>window.dunlinInjected=true</script>',
            citations: [{ id: sources.at(-1).id, quote: '交換周期は3か月。' }] },
        ] }] } : { conflict: '原本と食い違います', pages: [] };
      }
    }
    if (input.format.properties.issues) {
      const { items: [first, ...related] } = JSON.parse(input.messages[1].content);
      result = { issues: related.flatMap((next) => {
        if (first.type === 'source' && next.type === 'source') return [];
        const quote = (item) => item.text.split(/[\n[]/u)[0].trim();
        const refs = { first: { item: first.item, quote: quote(first) }, second: { item: next.item, quote: quote(next) } };
        return first.type !== next.type ? [{ ...refs, kind: 'source_disagreement', description: '資料との違い' }]
          : [{ ...refs, kind: 'duplicate', description: '同じ記述' }, { ...refs, kind: 'contradiction', description: 'ページ間の矛盾' },
            { ...refs, second: { ...refs.second, quote: '存在しない引用' }, kind: 'contradiction', description: '不正な引用' }];
      }) };
    }
    response.setHeader('content-type', 'application/json');
    response.end(JSON.stringify({ done: true, message: { content: JSON.stringify(result) } }));
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  t.after(() => new Promise((resolve) => server.close(resolve)));
  const run = (...args) => runNode(process.execPath, [cli, ...args], { cwd: root, env: { ...process.env, DUNLIN_GLOBAL_CONFIG: join(root, 'global.yaml') }, encoding: 'utf8', timeout: 10_000 });
  const json = async (...args) => JSON.parse((await run('--project', 'wiki', ...args, '--json')).stdout);
  await run('init', 'wiki', 'wiki', '--purpose', '設備保全の手順');
  await writeFile(join(root, 'manual.txt'), '交換手順を確認する。\n');
  const path = join(root, 'wiki', 'Dunlinfile.yaml');
  const config = parse(await readFile(path, 'utf8'));
  config.models.writer.model = 'testing';
  config.models.writer.base_url = 'http://127.0.0.1:' + server.address().port;
  await writeFile(path, stringify(config));
  const dry = await json('ingress', 'manual.txt', '--dry-run');
  assert.equal(dry.run_id, null);
  assert.equal(calls.length, 0);
  const result = await json('ingress', 'manual.txt');
  assert.equal(result.data.pages[0].id, 'maintenance');
  assert.match(await readFile(join(root, 'wiki', 'site', 'maintenance.html'), 'utf8'), /交換手順/u);
  const answer = await json('query', '交換手順');
  assert.equal(answer.data.pages[0].id, 'maintenance');
  assert.equal((await json('cite', 'maintenance', '1')).data.quote, '交換手順を確認する。');
  const saved = await json('save-answer', answer.run_id, 'faq', '交換の FAQ');
  assert.equal(saved.data.pages[0].id, 'faq');
  await json('undo', saved.run_id);
  const instructed = await json('instruct', 'maintenance', '短くしてください');
  assert.equal(instructed.data.pages[0].id, 'maintenance');
  await json('undo', instructed.run_id);
  const conflict = await json('instruct', 'maintenance', '交換周期を変更');
  assert.match(conflict.data.conflict, /食い違い/u);
  const supplemental = await json('instruct', 'maintenance', '交換周期を変更', '--supplement', '交換周期は3か月。', '--by', '点検担当');
  const citation = (await json('cite', 'maintenance', '2')).data;
  assert.equal(citation.quote, '交換周期は3か月。');
  assert.ok(citation.evidence.original.sha256);
  const published = await readFile(join(root, 'wiki', 'site', 'maintenance.html'), 'utf8');
  assert.match(published, /&lt;script&gt;/u);
  assert.ok(!published.includes('<script>'));
  assert.equal(await readFile(join(root, 'manual.txt'), 'utf8'), '交換手順を確認する。\n');
  assert.equal(parse(await readFile(path, 'utf8')).about.purpose, '設備保全の手順');
  await json('undo', supplemental.run_id);
  const count = calls.length;
  const issues = (await json('verify')).data.issues;
  assert.ok(issues.every(({ code }) => code !== 'CITATION_MISSING'));
  const orphan = issues.find(({ code }) => code === 'ORPHAN_PAGE');
  await json('issue', orphan.id, 'dismissed');
  assert.ok((await json('verify')).data.issues.every(({ id }) => id !== orphan.id));
  assert.equal(calls.length, count);
  await json('archive', 'maintenance');
  assert.equal((await json('query', '交換手順')).data.status, 'insufficient_evidence');
  await json('archive', '--restore', 'maintenance');
  const rules = await json('instruct', '--persistent', '手順は番号付きで書いてください');
  assert.match(await readFile(join(root, 'wiki', 'rules.md'), 'utf8'), /番号付き/u);
  await json('undo', rules.run_id);
  await json('save-answer', answer.run_id, 'faq', '交換の FAQ');
  await writeFile(join(root, 'new.txt'), '交換手順は見直し済み。\n');
  await json('ingress', 'new.txt', '--source-only');
  const inspected = (await json('verify', '--content')).data;
  const types = new Set(inspected.issues.map(({ kind }) => kind));
  for (const kind of ['contradiction', 'duplicate', 'source_disagreement']) assert.ok(types.has(kind), 'missing ' + kind);
  assert.ok(inspected.issues.every(({ description }) => description !== '不正な引用'));
  assert.ok(inspected.issues.filter(({ kind }) => kind).every(({ evidence }) =>
    evidence.length === 2 && evidence.every(({ quote, citations }) => quote && citations.length > 0)));
  const dismissed = inspected.issues.find(({ kind }) => kind === 'duplicate');
  await json('issue', dismissed.id, 'dismissed');
  const checked = (await json('verify', '--content')).data;
  assert.deepEqual(checked.review, { changed_items: 0, compared_pairs: 0, model_calls: 0 });
  assert.ok(checked.issues.every(({ id }) => id !== dismissed.id));
  assert.equal((await json('verify', '--content', '--all-issues')).data.issues.find(({ id }) => id === dismissed.id).state, 'dismissed');
  config.serve.port = 0;
  config.serve.access.auth = { type: 'token', token_env: 'DUNLIN_TEST_WIKI_TOKEN' };
  await writeFile(path, stringify(config));
  const site = spawn(process.execPath, [cli, '--project', 'wiki', 'serve'], { cwd: root,
    env: { ...process.env, DUNLIN_TEST_WIKI_TOKEN: 'local-wiki-token' }, stdio: ['ignore', 'ignore', 'pipe'] });
  const auth = { authorization: 'Basic ' + Buffer.from('dunlin:local-wiki-token').toString('base64') };
  const stopped = once(site, 'exit');
  try {
    let log = '';
    const ready = (async () => {
      for await (const chunk of site.stderr) {
        log += chunk;
        const url = /http:\/\/127\.0\.0\.1:\d+/u.exec(log)?.[0];
        if (url) return url;
      }
      throw new Error(log);
    })();
    const origin = await ready;
    const denied = await fetch(origin);
    assert.equal(denied.status, 401);
    await denied.arrayBuffer();
    const html = await (await fetch(origin + '/?page=maintenance', { headers: auth })).text();
    const token = /name="token" value="([^"]+)"/u.exec(html)[1];
    const bad = await fetch(origin + '/action', { method: 'POST', headers: { ...auth, origin: 'http://example.invalid' },
      body: new URLSearchParams({ token, action: 'query', value: '交換手順' }) });
    assert.equal(bad.status, 403);
    const form = new FormData();
    form.set('token', token);
    form.set('action', 'ingress');
    form.set('upload', new Blob(['交換手順を確認する。\n']), 'pane-manual.txt');
    pauseModel = Promise.withResolvers();
    const added = await fetch(origin + '/action', { method: 'POST', headers: { ...auth, origin }, body: form, redirect: 'manual' });
    assert.equal(added.status, 303);
    const workPage = new URL(added.headers.get('location'), origin);
    const workId = workPage.searchParams.get('work');
    const waitFor = async (url, pattern) => {
      for (let attempt = 0; attempt < 50; attempt++) {
        const html = await (await fetch(url, { headers: auth })).text();
        if (pattern.test(html)) return html;
        await delay(40);
      }
      throw new Error('Timed out waiting for the Wiki work page.');
    };
    await waitFor(workPage, /data-work-status="running".*ページを作成中/us);
    const progress = (await json('progress', workId)).data;
    assert.equal(progress.stage, 'writing_pages');
    assert.deepEqual(progress.connection, { address: '127.0.0.1', host: '127.0.0.1', origin });
    const waiting = await fetch(origin + '/action', { method: 'POST', headers: { ...auth, origin },
      body: new URLSearchParams({ token, action: 'verify' }), redirect: 'manual' });
    const nextPage = new URL(waiting.headers.get('location'), origin);
    await waitFor(nextPage, /data-work-status="queued"/u);
    pauseModel.resolve();
    assert.match(await waitFor(workPage, /data-work-status="succeeded"/u), /pane-manual.txt/u);
    await waitFor(nextPage, /data-work-status="succeeded"/u);
  } finally { pauseModel?.resolve(); site.kill(); await stopped; }
  await run('init', 'para', 'para', '--purpose', '設備保全の記録');
  const paraPath = join(root, 'para', 'Dunlinfile.yaml');
  const para = parse(await readFile(paraPath, 'utf8'));
  para.models.writer = config.models.writer;
  await writeFile(paraPath, stringify(para));
  const paraRun = async (...args) => JSON.parse((await run('--project', 'para', ...args, '--json')).stdout);
  await paraRun('ingress', 'manual.txt');
  const archived = await paraRun('archive', 'Projects/maintenance');
  const archiveFile = join(root, 'para', 'site', 'Archives', 'Projects', 'maintenance.html');
  assert.match(await readFile(archiveFile, 'utf8'), /交換手順/u);
  assert.match(await readFile(join(root, 'para', 'site', 'Projects', 'maintenance.html'), 'utf8'), /アーカイブで読む/u);
  await paraRun('undo', archived.run_id);
  await assert.rejects(readFile(archiveFile, 'utf8'), { code: 'ENOENT' });
});
