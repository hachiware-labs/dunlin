import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

import { Client } from '@modelcontextprotocol/client';
import { StdioClientTransport } from '@modelcontextprotocol/client/stdio';
import { parse, stringify } from 'yaml';

const cli = fileURLToPath(new URL('../cli.mjs', import.meta.url));
const runNode = promisify(execFile);

test('initializes the four project types through the public CLI', async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-init-types-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const run = (...args) => runNode(process.execPath, [cli, ...args], { cwd: root, env: { ...process.env, DUNLIN_GLOBAL_CONFIG: join(root, 'global.yaml') }, encoding: 'utf8', timeout: 10_000 });
  for (const type of ['mcp', 'rag', 'wiki', 'para']) {
    const searchProject = ['mcp', 'rag'].includes(type);
    const purpose = searchProject ? [] : ['--purpose', '運用手順を整理する'];
    await run('init', type, type, ...purpose);
    const config = parse(await readFile(join(root, type, 'Dunlinfile.yaml'), 'utf8'));
    assert.deepEqual(config.init, { version: 1, type });
    if (searchProject) {
      assert.deepEqual(config.source.modules[0], { use: 'source.filesystem', with: { path: './docs', exclude: [] } });
      assert.deepEqual(config.transform.modules[1], { use: 'chunk.length', with: { unit: 'characters', size: 800, overlap: 100 } });
      assert.deepEqual(config.index.modules[0], { use: 'index.keyword', with: { tokenizer: 'lindera', user_dictionary: null } });
      assert.equal(config.search.modules.at(-1).use, 'context.assemble');
      if (type === 'mcp') {
        assert.deepEqual(config.serve, { transport: 'stdio' });
        assert.equal(config.models, undefined);
        assert.equal(config.ask, undefined);
      } else {
        assert.equal(config.models.answerer.model, null);
        assert.equal(config.ask.modules[0].use, 'answer.generate');
        assert.equal(config.serve.port, 4318);
        assert.deepEqual(config.serve.endpoints, ['responses', 'search', 'mcp']);
      }
    } else {
      assert.equal(config.about.purpose, '運用手順を整理する');
      assert.equal(config.models.writer.model, null);
    }
  }
  const legacyPath = join(root, 'mcp', 'Dunlinfile.yaml');
  const legacy = parse(await readFile(legacyPath, 'utf8'));
  legacy.init.type = 'rag';
  await writeFile(legacyPath, stringify(legacy));
  const validated = JSON.parse((await run('--project', 'mcp', 'validate', '--json')).stdout);
  assert.equal(validated.data.type, 'mcp');
  assert.equal(validated.warnings[0].code, 'LEGACY_PROJECT_TYPE');
  assert.equal(parse(await readFile(legacyPath, 'utf8')).init.type, 'rag');
  await assert.rejects(run('--project', 'wiki', 'build'), { code: 2, stderr: /requires a RAG or MCP project/u });
  await assert.rejects(run('--project', 'mcp', 'ask', 'question'), { code: 2, stderr: /ask requires a RAG project/u });
  await assert.rejects(run('init', '--preset', 'rag'), { code: 2 });
});

test('copies global model defaults into new projects and accepts explicit overrides', { timeout: 30_000 }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-global-config-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const path = join(root, 'settings', 'config.yaml');
  const env = { ...process.env, DUNLIN_GLOBAL_CONFIG: path, OPENAI_API_KEY: '' };
  const run = (...args) => runNode(process.execPath, [cli, ...args], { cwd: root, env, encoding: 'utf8', timeout: 10_000 });
  const json = async (...args) => JSON.parse((await run(...args, '--json')).stdout);
  const read = (project) => readFile(join(root, project, 'Dunlinfile.yaml'), 'utf8');
  assert.deepEqual((await json('config', 'list')).data, { path, version: 1 });
  await json('config', 'set', 'embedding', '--model', 'local-embed');
  await json('config', 'set', 'llm', '--provider', 'openai', '--model', 'answer-model');

  await run('init', 'mcp', 'kb');
  const mcp = parse(await read('kb'));
  assert.deepEqual(mcp.models, { embedder: {
    provider: 'ollama', kind: 'embedding', base_url: 'http://localhost:11434', model: 'local-embed',
  } });
  assert.deepEqual(mcp.index.modules.map(({ use }) => use), ['index.keyword', 'index.vector']);
  assert.deepEqual(mcp.index.modules[1].with, { model: 'embedder' });
  assert.deepEqual(mcp.search.modules.map(({ use }) => use),
    ['query.normalize', 'retrieval.keyword', 'retrieval.vector', 'fusion.rrf', 'context.assemble']);
  assert.deepEqual(mcp.search.modules[2].with, { model: 'embedder', top_k: 10 });
  assert.deepEqual(mcp.search.modules[3].with, { k: 60 });

  await run('init', 'rag', 'qa');
  const rag = await read('qa');
  assert.deepEqual(parse(rag).models, { answerer: {
    provider: 'openai', kind: 'chat', base_url: 'https://api.openai.com/v1', api_key_env: 'OPENAI_API_KEY', model: 'answer-model',
  }, ...mcp.models });
  assert.equal((await json('--project', 'qa', 'validate')).data.valid, true);
  for (const type of ['wiki', 'para']) {
    await run('init', type, type);
    const config = parse(await read(type));
    assert.deepEqual(config.models, { writer: parse(rag).models.answerer });
  }

  await run('init', 'rag', 'custom', '--embedding-model', 'custom-embed', '--embedding-provider', 'openai',
    '--llm-model', 'custom-chat', '--llm-provider', 'strata');
  const custom = parse(await read('custom'));
  assert.equal(custom.models.embedder.provider, 'openai');
  assert.equal(custom.models.embedder.model, 'custom-embed');
  assert.equal(custom.models.embedder.api_key_env, 'OPENAI_API_KEY');
  assert.deepEqual(custom.models.answerer, {
    provider: 'strata', kind: 'chat', base_url: 'http://127.0.0.1:8080/v1', model: 'custom-chat',
  });
  await run('init', 'rag', 'keyword', '--no-embedding', '--no-llm');
  const keyword = parse(await read('keyword'));
  assert.equal(keyword.models.answerer.model, null);
  assert.equal(keyword.models.embedder, undefined);
  assert.equal(keyword.index.modules.length, 1);
  assert.equal(keyword.search.modules.length, 3);

  await assert.rejects(run('init', 'mcp', 'bad', '--llm-model', 'unsupported'), { code: 2 });
  await assert.rejects(run('init', 'wiki', 'bad', '--embedding-model', 'unsupported'), { code: 2 });
  await assert.rejects(run('init', 'rag', 'bad', '--embedding-provider', 'strata'), { code: 2 });
  await assert.rejects(run('init', 'rag', 'bad', '--embedding-provider', 'openai'), { code: 2 });
  await assert.rejects(run('init', 'rag', 'bad', '--no-llm', '--llm-model', 'conflict'), { code: 2 });
  await assert.rejects(read('bad'), { code: 'ENOENT' });

  await json('config', 'unset', 'embedding');
  assert.deepEqual((await json('config', 'list')).data.llm, { provider: 'openai', model: 'answer-model' });
  await run('init', 'mcp', 'next');
  assert.equal(parse(await read('next')).models, undefined);
  assert.equal(await read('qa'), rag);
});

test('searches Japanese questions and pins the user dictionary to its build', { timeout: 35_000 }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-japanese-search-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const env = { ...process.env, DUNLIN_GLOBAL_CONFIG: join(root, 'global.yaml') };
  const run = (...args) => runNode(process.execPath, [cli, ...args], { cwd: root, env, encoding: 'utf8', timeout: 10_000 });
  const json = async (...args) => JSON.parse((await run('--project', 'kb', ...args, '--json')).stdout);
  await run('init', 'mcp', 'kb');
  const docs = join(root, 'kb', 'docs');
  const configPath = join(root, 'kb', 'Dunlinfile.yaml');
  const compound = '超伝導異常監視モジュール';
  await writeFile(join(docs, 'guide.txt'), '保守手順\nメカニカルシールは6か月ごとに交換してください。\nポンプは毎月の第1営業日に点検を実施します。\n');
  await writeFile(join(docs, 'module.txt'), compound + 'を交換します。\n');
  await writeFile(join(docs, 'general.txt'), '超伝導を研究する。異常を検知する。監視を開始する。モジュールを組み立てる。\n');
  await writeFile(join(docs, 'close.txt'), '電源を遮断して計器を確認する。\n');
  await writeFile(join(docs, 'reversed.txt'), '計器を確認して電源を遮断する。\n');
  const baseline = (await json('build')).data;
  const query = 'メカニカルシールの交換周期は';
  for (const question of [query, 'ポンプの点検はいつ実施しますか', '手順']) {
    assert.equal((await json('search', question)).data.results[0].document_id, 'docs:guide.txt');
  }
  assert.deepEqual((await json('search', '人工衛星の軌道は')).data.results, []);
  const sequence = (await json('search', '電源を遮断して計器を確認する')).data;
  assert.equal(sequence.results[0].document_id, 'docs:close.txt');
  assert.ok(sequence.results[0].score > sequence.results.find(({ document_id }) => document_id === 'docs:reversed.txt').score);
  await json('eval-data', 'add', '--question', query, '--doc', 'docs:guide.txt', '--quote', '6か月ごとに交換');
  assert.equal((await json('eval')).data.scores['evidence_recall@context'], 1);
  const original = (await json('search', compound)).data;
  assert.ok(original.results.some(({ document_id }) => document_id === 'docs:general.txt'));

  const config = parse(await readFile(configPath, 'utf8'));
  config.index.modules[0].with.user_dictionary = './user.csv';
  await writeFile(configPath, stringify(config));
  const dictionaryPath = join(root, 'kb', 'user.csv');
  await writeFile(dictionaryPath, compound + ',名詞,チョウデンドウイジョウカンシモジュール\n');
  const withDictionary = (await json('build', '--from', 'index')).data;
  assert.notEqual(withDictionary.build_id, baseline.build_id);
  assert.equal(withDictionary.transform_id, baseline.transform_id);
  await json('tag', 'compound', withDictionary.build_id);
  const focused = (await json('search', compound)).data;
  assert.deepEqual(focused.results.map(({ document_id }) => document_id), ['docs:module.txt']);
  assert.notEqual(focused.search_config_id, original.search_config_id);

  await writeFile(dictionaryPath, '遮断計器,名詞,シャダンケイキ\n');
  assert.deepEqual((await json('search', compound)).data, focused);
  await json('build', '--from', 'index');
  assert.deepEqual((await json('search', compound, '--build', 'compound')).data, focused);
  assert.ok((await json('search', compound)).data.results.some(({ document_id }) => document_id === 'docs:general.txt'));

  await writeFile(dictionaryPath, '領域,名詞\n');
  await assert.rejects(json('build', '--from', 'index'), (error) =>
    error.code === 2 && JSON.parse(error.stdout).errors[0].code === 'INVALID_USER_DICTIONARY');
});

test('runs only the selected stages and indexes the saved chunk ranges', { timeout: 30_000 }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-stages-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const run = (...args) => runNode(process.execPath, [cli, ...args], { cwd: root, env: { ...process.env, DUNLIN_GLOBAL_CONFIG: join(root, 'global.yaml') }, encoding: 'utf8', timeout: 10_000 });
  const json = async (...args) => JSON.parse((await run('--project', 'kb', ...args, '--json')).stdout);
  await run('init', 'mcp', 'kb');
  const path = join(root, 'kb', 'Dunlinfile.yaml');
  const doc = join(root, 'kb', 'docs', 'guide.txt');
  await writeFile(doc, '準備の案内\n作業手順はこちら\n後片付け\n');
  await assert.rejects(json('transform'), { code: 3 });
  await assert.rejects(json('index'), { code: 3 });
  const source = (await json('source')).data;
  assert.deepEqual(Object.keys(source.stages), ['source']);
  const config = parse(await readFile(path, 'utf8'));
  config.transform.modules[1].with.size = 10;
  config.transform.modules[1].with.overlap = 0;
  await writeFile(path, stringify(config));
  await writeFile(doc, '資料を差し替えました\n');
  const transform = (await json('transform')).data;
  assert.deepEqual(Object.keys(transform.stages), ['transform']);
  const index = (await json('index')).data;
  assert.deepEqual(Object.keys(index.stages), ['index']);
  assert.equal(index.serving_changed, false);
  await assert.rejects(json('search', '手順'), { code: 3 });
  const resumed = (await json('build', '--from', 'transform')).data;
  assert.deepEqual(Object.keys(resumed.stages), ['transform', 'index']);
  assert.equal(resumed.source_id, source.stages.source.id);
  assert.equal(resumed.transform_id, transform.stages.transform.id);
  assert.equal(resumed.build_id, index.build_id);
  assert.equal(resumed.stages.index.reused, true);
  const result = (await json('search', '手順')).data;
  assert.equal(result.results[0].evidence.range.endLine, 2);
  assert.ok(result.results[0].evidence.byteRange.end < Buffer.byteLength('準備の案内\n作業手順はこちら\n後片付け\n'));

  const partial = (await json('build', '--until', 'source')).data;
  assert.notEqual(partial.stages.source.id, resumed.source_id);
  assert.equal((await json('status')).data.build_id, resumed.build_id);
  const stale = await json('index');
  assert.equal(stale.warnings[0].code, 'STALE_STAGE');
  config.index.modules[0].with.typo = true;
  config.transform.modules[1].with.size = 'ten';
  await writeFile(path, stringify(config));
  await assert.rejects(json('build'), (error) => {
    assert.equal(error.code, 2);
    const result = JSON.parse(error.stdout);
    assert.deepEqual(result.errors.map(({ code }) => code), ['ARG_TYPE', 'ARG_UNKNOWN']);
    assert.ok(result.errors.every(({ file, line }) => file.endsWith('Dunlinfile.yaml') && line > 0));
    return true;
  });
});

test('initializes, builds, searches and serves the same Japanese evidence over MCP', { timeout: 30_000 }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-project-'));
  const client = new Client({ name: 'project-test', version: '1.0.0' });
  t.after(async () => {
    await client.close();
    await rm(root, { recursive: true, force: true, maxRetries: 5 });
  });
  const project = join(root, 'knowledge');
  const docs = join(root, 'manuals');
  await mkdir(docs);
  await writeFile(join(docs, 'guide.md'), '# ガイド\n作業手順を確認してください。\n');
  await writeFile(join(docs, 'notes.bin'), 'not a supported standard file');
  const run = (cwd, ...args) => runNode(process.execPath, [cli, ...args], { cwd, env: { ...process.env, DUNLIN_GLOBAL_CONFIG: join(root, 'global.yaml') }, timeout: 10_000, encoding: 'utf8' });
  const json = async (cwd, ...args) => JSON.parse((await run(cwd, ...args, '--json')).stdout);

  await run(root, 'init', 'mcp', './knowledge', '--source', './manuals');
  const config = await readFile(join(project, 'Dunlinfile.yaml'), 'utf8');
  assert.equal(parse(config).init.type, 'mcp');
  await assert.rejects(run(root, 'init', 'mcp', './knowledge'), { code: 2 });
  assert.equal(await readFile(join(project, 'Dunlinfile.yaml'), 'utf8'), config);
  assert.equal((await json(project, 'status')).data.build_id, null);
  await assert.rejects(run(project, 'search', '手順', '--json'), (error) => {
    assert.equal(error.code, 3);
    assert.equal(JSON.parse(error.stdout).errors[0].code, 'BUILD_REQUIRED');
    return true;
  });
  await assert.rejects(run(project, 'serve'), (error) => error.code === 3 && error.stdout === '');
  await assert.rejects(run(project, 'build', '--json'), (error) => {
    assert.equal(error.code, 2);
    const result = JSON.parse(error.stdout);
    assert.equal(result.errors[0].code, 'UNSUPPORTED_FILES');
    assert.equal(result.data.skipped[0].path, 'notes.bin');
    return true;
  });
  assert.equal((await json(project, 'status')).data.build_id, null);

  const build = (await json(project, 'build', '--skip-unsupported')).data;
  assert.equal(build.documents, 1);
  assert.equal(build.skipped[0].path, 'notes.bin');
  assert.equal((await json(project, 'status')).data.build_id, build.build_id);
  const repeated = (await json(project, 'build', '--skip-unsupported')).data;
  assert.equal(repeated.build_id, build.build_id);
  assert.equal(repeated.reused, true);
  assert.equal(repeated.serving_changed, false);
  const nested = join(project, 'nested');
  await mkdir(nested);
  const result = (await json(nested, 'search', '手順')).data;
  assert.equal(result.build_id, build.build_id);
  assert.equal(result.results[0].evidence.source.objectId, 'guide.md');
  assert.match(result.context.text, /作業手順を確認/u);

  const launch = JSON.parse((await run(root, '--project', project, 'serve', '--print-config')).stdout);
  await client.connect(new StdioClientTransport({ ...launch, cwd: root, stderr: 'ignore' }));
  const { structuredContent, isError } = await client.callTool({ name: 'knowledge.search', arguments: { query: '手順' } });
  assert.ok(!isError);
  assert.equal(structuredContent.build_id, result.build_id);
  assert.equal(structuredContent.search_config_id, result.search_config_id);
  assert.deepEqual(structuredContent, result);
  const read = await client.callTool({ name: 'knowledge.read', arguments: { evidence: structuredContent.results[0].evidence } });
  assert.ok(!read.isError);
  assert.match(read.structuredContent.content, /作業手順を確認/u);

  await writeFile(join(docs, 'guide.md'), '# ガイド\n保全手順を更新しました。\n');
  const updated = (await json(project, 'build', '--skip-unsupported')).data;
  const current = await client.callTool({ name: 'knowledge.search', arguments: { query: '手順' } });
  assert.equal(current.structuredContent.build_id, updated.build_id);
  assert.match(current.structuredContent.context.text, /保全手順/u);

  await writeFile(join(docs, 'guide.md'), Buffer.from([0xff]));
  await assert.rejects(run(project, 'build', '--skip-unsupported'), { code: 1 });
  assert.equal((await json(project, 'search', '手順')).data.build_id, updated.build_id);

  await writeFile(join(docs, 'guide.md'), '交換手順を変更しました。\n');
  const edited = parse(config);
  edited.index.modules[0].with = { tokenizer: 'word', short_query: 'none' };
  await writeFile(join(project, 'Dunlinfile.yaml'), stringify(edited));
  const incompatible = (await json(project, 'build', '--skip-unsupported')).data;
  assert.notEqual(incompatible.build_id, updated.build_id);
  const pinned = await client.callTool({ name: 'knowledge.search', arguments: { query: '手順' } });
  assert.equal(pinned.structuredContent.build_id, updated.build_id);
  assert.match(pinned.structuredContent.context.text, /保全手順/u);
});
