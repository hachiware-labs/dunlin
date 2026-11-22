import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { link, mkdir, mkdtemp, readdir, readFile, rm, stat, writeFile } from 'node:fs/promises';
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
  const root = await mkdtemp(join(tmpdir(), 'knowledge-nest-init-types-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const run = (...args) => runNode(process.execPath, [cli, ...args], { cwd: root, env: { ...process.env, KNOWLEDGE_NEST_GLOBAL_CONFIG: join(root, 'global.yaml') }, encoding: 'utf8', timeout: 10_000 });
  for (const type of ['mcp', 'rag', 'wiki', 'para']) {
    const searchProject = ['mcp', 'rag'].includes(type);
    const purpose = searchProject ? [] : ['--purpose', '運用手順を整理する'];
    await run('init', type, type, ...purpose);
    const config = parse(await readFile(join(root, type, 'KnowledgeNestfile.yaml'), 'utf8'));
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
  const legacyPath = join(root, 'mcp', 'KnowledgeNestfile.yaml');
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
  const root = await mkdtemp(join(tmpdir(), 'knowledge-nest-global-config-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const path = join(root, 'settings', 'config.yaml');
  const env = { ...process.env, KNOWLEDGE_NEST_GLOBAL_CONFIG: path, OPENAI_API_KEY: '' };
  const run = (...args) => runNode(process.execPath, [cli, ...args], { cwd: root, env, encoding: 'utf8', timeout: 10_000 });
  const json = async (...args) => JSON.parse((await run(...args, '--json')).stdout);
  const read = (project) => readFile(join(root, project, 'KnowledgeNestfile.yaml'), 'utf8');
  assert.deepEqual((await json('config', 'list')).data, { path, version: 1 });
  await json('config', 'set', 'embedding', '--model', 'local-embed');
  await json('config', 'set', 'llm', '--provider', 'openai', '--model', 'answer-model');

  await run('init', 'mcp', 'kb');
  const mcp = parse(await read('kb'));
  assert.equal(mcp.models, undefined);
  assert.deepEqual(mcp.index.modules.map(({ use }) => use), ['index.keyword']);
  await run('init', 'mcp', 'hybrid', '--embedding-model', 'local-embed');
  const hybrid = parse(await read('hybrid'));
  assert.deepEqual(hybrid.models, { embedder: {
    provider: 'ollama', kind: 'embedding', base_url: 'http://localhost:11434', model: 'local-embed',
  } });
  assert.deepEqual(hybrid.index.modules.map(({ use }) => use), ['index.keyword', 'index.vector']);
  assert.deepEqual(hybrid.index.modules[1].with, { model: 'embedder' });
  assert.deepEqual(hybrid.search.modules.map(({ use }) => use),
    ['query.normalize', 'retrieval.keyword', 'retrieval.vector', 'fusion.rrf', 'context.assemble']);
  assert.deepEqual(hybrid.search.modules[2].with, { model: 'embedder', top_k: 10 });
  assert.deepEqual(hybrid.search.modules[3].with, { k: 60 });

  await run('init', 'rag', 'qa');
  const rag = await read('qa');
  assert.deepEqual(parse(rag).models, { answerer: {
    provider: 'openai', kind: 'chat', base_url: 'https://api.openai.com/v1', api_key_env: 'OPENAI_API_KEY', model: 'answer-model',
  }, ...hybrid.models });
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

test('copies the global OCR selection into each project type and accepts init overrides', { timeout: 30_000 }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'knowledge-nest-global-ocr-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const path = join(root, 'global.yaml');
  const ocrHome = join(root, 'ocr');
  const env = { ...process.env, KNOWLEDGE_NEST_GLOBAL_CONFIG: path, KNOWLEDGE_NEST_OCR_HOME: ocrHome };
  const run = (...args) => runNode(process.execPath, [cli, ...args], { cwd: root, env, encoding: 'utf8', timeout: 10_000 });
  const json = async (...args) => JSON.parse((await run(...args, '--json')).stdout);
  const read = async (name) => parse(await readFile(join(root, name, 'KnowledgeNestfile.yaml'), 'utf8'));
  const loader = (config) => (config.source ?? config.ingress).modules.find(({ use }) => use === 'loader.auto');

  await json('config', 'set', 'llm', '--model', 'local-writer');
  const selected = await json('config', 'set', 'ocr', '--tool', 'lightonocr');
  assert.deepEqual(selected.data, { path, version: 1, llm: { provider: 'ollama', model: 'local-writer' }, ocr: { tool: 'lightonocr' } });
  assert.deepEqual((await json('config', 'list')).data, selected.data);
  assert.match((await run('config', 'list')).stdout, /^ocr +lightonocr$/mu);
  const listed = (await json('add', '--list')).data;
  assert.equal(listed.some(({ name }) => name === 'ocr'), false);
  const { name, version } = listed.find(({ name }) => name === 'lightonocr');
  const oar = listed.find(({ name }) => name === 'oar-ocr');

  for (const type of ['rag', 'wiki', 'para']) {
    await run('init', type, type);
    const config = await read(type);
    assert.equal(loader(config).with.ocr, 'loader.ocr');
    assert.deepEqual(config.tools, [{ name, version }]);
    assert.equal((await json('--project', type, 'validate')).data.valid, true);
  }
  await run('init', 'mcp', 'mcp');
  assert.equal(loader(await read('mcp')).with.ocr, 'none');
  await run('init', 'mcp', 'mcp-ocr', '--ocr');
  assert.equal(loader(await read('mcp-ocr')).with.ocr, 'loader.ocr');
  assert.equal((await read('wiki')).models.writer.model, 'local-writer');
  await run('init', 'rag', 'skip-ocr', '--no-ocr');
  assert.equal(loader(await read('skip-ocr')).with.ocr, 'none');
  assert.equal((await read('skip-ocr')).tools, undefined);
  await assert.rejects(run('init', 'rag', 'conflict', '--ocr', '--no-ocr'), { code: 2 });
  await assert.rejects(run('config', 'set', 'ocr', '--tool', 'tesseract-js'), { code: 2 });
  await assert.rejects(run('config', 'set', 'ocr', '--model', 'unsupported'), { code: 2 });
  await assert.rejects(run('config', 'list', '--tool', 'lightonocr'), { code: 2 });
  await assert.rejects(run('config', 'unset', 'ocr', '--tool', 'lightonocr'), { code: 2 });

  // --ocr-tool chooses the tool for one project and turns OCR on.
  await run('init', 'rag', 'chosen', '--ocr-tool', 'oar-ocr');
  assert.deepEqual((await read('chosen')).tools, [{ name: 'oar-ocr', version: oar.version }]);
  await assert.rejects(run('init', 'rag', 'bad-tool', '--ocr-tool', 'tesseract-js'), { code: 2 });
  await assert.rejects(run('init', 'rag', 'off-tool', '--ocr-tool', 'oar-ocr', '--no-ocr'), { code: 2 });

  await run('config', 'unset', 'ocr');
  assert.equal((await json('config', 'list')).data.ocr, undefined);
  await run('init', 'rag', 'default');
  await run('init', 'rag', 'enabled', '--ocr');
  assert.equal(loader(await read('default')).with.ocr, 'none');
  assert.equal(loader(await read('enabled')).with.ocr, 'loader.ocr');
  assert.equal(loader(await read('rag')).with.ocr, 'loader.ocr');
  // Without a selection, OCR uses oar-ocr.
  assert.deepEqual((await read('enabled')).tools, [{ name: 'oar-ocr', version: oar.version }]);
  assert.deepEqual((await json('config', 'set', 'ocr')).data.ocr, { tool: 'oar-ocr' });

  // The former tool name stays valid, and a project uses one OCR tool.
  const configPath = join(root, 'enabled', 'KnowledgeNestfile.yaml');
  const text = await readFile(configPath, 'utf8');
  await writeFile(configPath, text.replace(/tools:[\s\S]*$/u, `tools:
  - name: ocr
    version: ${version}
`));
  assert.equal((await json('--project', 'enabled', 'validate')).data.valid, true);
  await writeFile(configPath, text.replace(/tools:[\s\S]*$/u, `tools:
  - name: oar-ocr
    version: ${oar.version}
  - name: lightonocr
    version: ${version}
`));
  const conflict = await run('--project', 'enabled', 'validate', '--json').then((result) => JSON.parse(result.stdout), (error) => JSON.parse(error.stdout));
  assert.deepEqual(conflict.errors.map(({ code, section, expected }) => [code, section, expected]), [['ARG_CONFLICT', 'tools', 'One OCR tool']]);
  await assert.rejects(stat(ocrHome), { code: 'ENOENT' });
});

test('searches Japanese questions and pins the user dictionary to its build', { timeout: 35_000 }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'knowledge-nest-japanese-search-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const env = { ...process.env, KNOWLEDGE_NEST_GLOBAL_CONFIG: join(root, 'global.yaml') };
  const run = (...args) => runNode(process.execPath, [cli, ...args], { cwd: root, env, encoding: 'utf8', timeout: 10_000 });
  const json = async (...args) => JSON.parse((await run('--project', 'kb', ...args, '--json')).stdout);
  await run('init', 'mcp', 'kb');
  const docs = join(root, 'kb', 'docs');
  const configPath = join(root, 'kb', 'KnowledgeNestfile.yaml');
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
  const root = await mkdtemp(join(tmpdir(), 'knowledge-nest-stages-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const run = (...args) => runNode(process.execPath, [cli, ...args], { cwd: root, env: { ...process.env, KNOWLEDGE_NEST_GLOBAL_CONFIG: join(root, 'global.yaml') }, encoding: 'utf8', timeout: 10_000 });
  const json = async (...args) => JSON.parse((await run('--project', 'kb', ...args, '--json')).stdout);
  await run('init', 'mcp', 'kb');
  const path = join(root, 'kb', 'KnowledgeNestfile.yaml');
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
    assert.ok(result.errors.every(({ file, line }) => file.endsWith('KnowledgeNestfile.yaml') && line > 0));
    return true;
  });
});

test('shares immutable segment files while preserving the previous build', { timeout: 30_000 }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'knowledge-nest-index-segments-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const env = { ...process.env, KNOWLEDGE_NEST_GLOBAL_CONFIG: join(root, 'global.yaml') };
  const json = async (...args) => JSON.parse((await runNode(process.execPath, [cli, ...args, '--json'],
    { cwd: root, env, encoding: 'utf8', timeout: 10_000 })).stdout);
  await json('init', 'rag', '.');
  const document = join(root, 'docs', 'guide.txt');
  await writeFile(document, '交換周期は6か月です。\n');
  await writeFile(join(root, 'docs', 'reference.txt'), '検査には絶縁手袋を使います。\n');
  const initial = await json('build');
  const original = (await json('search', '交換周期')).data;
  assert.match(original.context.text, /6か月/u);

  const artifacts = join(root, '.knowledge-nest', 'artifacts');
  const prior = join(artifacts, initial.run_id, '.knowledge-nest-index-v1');
  const files = await readdir(prior);
  const segment = files.find((name) => /^[0-9a-f]{32}\.idx$/u.test(name));
  assert.ok(segment);
  const priorIndex = join(prior, segment);
  const probe = join(root, 'link-probe');
  const supportsLinks = await link(priorIndex, probe).then(() => true, (failure) => {
    if (['EACCES', 'EINVAL', 'EMLINK', 'ENOSYS', 'ENOTSUP', 'EOPNOTSUPP', 'EPERM', 'EXDEV'].includes(failure.code)) return false;
    throw failure;
  });
  if (supportsLinks) await rm(probe);
  const metadata = await readFile(join(prior, 'meta.json'));
  const managed = await readFile(join(prior, '.managed.json'));

  await writeFile(document, '交換周期は3か月です。\n');
  const updated = await json('build');
  const next = join(artifacts, updated.run_id, '.knowledge-nest-index-v1');
  const [oldFile, newFile] = await Promise.all([stat(priorIndex, { bigint: true }), stat(join(next, segment), { bigint: true })]);
  if (supportsLinks) {
    assert.equal(newFile.ino, oldFile.ino);
    assert.ok(newFile.nlink > 1n);
  } else {
    assert.deepEqual(await readFile(join(next, segment)), await readFile(priorIndex));
  }
  assert.deepEqual(await readFile(join(prior, 'meta.json')), metadata);
  assert.deepEqual(await readFile(join(prior, '.managed.json')), managed);
  const current = (await json('search', '交換周期')).data;
  assert.equal(current.build_id, updated.data.build_id);
  assert.match(current.context.text, /3か月/u);
  assert.deepEqual((await json('search', '交換周期', '--build', initial.data.build_id)).data, original);
});

test('initializes, builds, searches and serves the same Japanese evidence over MCP', { timeout: 30_000 }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'knowledge-nest-project-'));
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
  const run = (cwd, ...args) => runNode(process.execPath, [cli, ...args], { cwd, env: { ...process.env, KNOWLEDGE_NEST_GLOBAL_CONFIG: join(root, 'global.yaml') }, timeout: 10_000, encoding: 'utf8' });
  const json = async (cwd, ...args) => JSON.parse((await run(cwd, ...args, '--json')).stdout);

  // A missing source folder stops init; a folder removed after init stops build with its path.
  const unknown = await run(root, 'init', 'mcp', './nowhere-kb', '--source', './nowhere').catch((error) => error);
  assert.equal(unknown.code, 2);
  assert.match(unknown.stderr, /^Source folder not found: .*nowhere\. Pass an existing folder to --source\./mu);
  await assert.rejects(stat(join(root, 'nowhere-kb', 'KnowledgeNestfile.yaml')), { code: 'ENOENT' });
  await mkdir(join(root, 'removed'));
  await run(root, 'init', 'mcp', './removed-kb', '--source', './removed');
  await rm(join(root, 'removed'), { recursive: true });
  const removed = await run(join(root, 'removed-kb'), 'build').catch((error) => error);
  assert.equal(removed.code, 3);
  assert.match(removed.stderr, /^Source folder not found: .*removed\. Create it, or set the path of source\.filesystem in KnowledgeNestfile\.yaml\./mu);

  await run(root, 'init', 'mcp', './knowledge', '--source', './manuals');
  const config = await readFile(join(project, 'KnowledgeNestfile.yaml'), 'utf8');
  assert.equal(parse(config).init.type, 'mcp');
  await assert.rejects(run(root, 'init', 'mcp', './knowledge'), { code: 2 });
  assert.equal(await readFile(join(project, 'KnowledgeNestfile.yaml'), 'utf8'), config);
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
  // An unknown build reference names itself instead of asking for a first build.
  await assert.rejects(run(project, 'use', 'no-such-build', '--json'), (error) => {
    assert.equal(error.code, 3);
    const failure = JSON.parse(error.stdout).errors[0];
    assert.equal(failure.code, 'BUILD_NOT_FOUND');
    assert.match(failure.message, /^Unknown build: no-such-build\./u);
    return true;
  });
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

  // Without --json, print-config prints an mcpServers entry to paste into a client's settings.
  const printed = await run(root, '--project', project, 'serve', '--print-config');
  const servers = JSON.parse(printed.stdout).mcpServers;
  assert.deepEqual(Object.keys(servers), ['knowledge-nest-knowledge']);
  assert.match(printed.stderr, /mcpServers/u);
  const launch = servers['knowledge-nest-knowledge'];
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
  await writeFile(join(project, 'KnowledgeNestfile.yaml'), stringify(edited));
  const incompatible = (await json(project, 'build', '--skip-unsupported')).data;
  assert.notEqual(incompatible.build_id, updated.build_id);
  const pinned = await client.callTool({ name: 'knowledge.search', arguments: { query: '手順' } });
  assert.equal(pinned.structuredContent.build_id, updated.build_id);
  assert.match(pinned.structuredContent.context.text, /保全手順/u);
});
