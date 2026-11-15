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
  const run = (...args) => runNode(process.execPath, [cli, ...args], { cwd: root, encoding: 'utf8', timeout: 10_000 });
  for (const type of ['mcp', 'rag', 'wiki', 'para']) {
    const searchProject = ['mcp', 'rag'].includes(type);
    const purpose = searchProject ? [] : ['--purpose', '運用手順を整理する'];
    await run('init', type, type, ...purpose);
    const config = parse(await readFile(join(root, type, 'Dunlinfile.yaml'), 'utf8'));
    assert.deepEqual(config.init, { version: 1, type });
    if (searchProject) {
      assert.deepEqual(config.source.modules[0], { use: 'source.filesystem', with: { path: './docs', exclude: [] } });
      assert.deepEqual(config.transform.modules[1], { use: 'chunk.length', with: { unit: 'characters', size: 800, overlap: 100 } });
      assert.deepEqual(config.index.modules[0], { use: 'index.keyword', with: { tokenizer: 'trigram', short_query: 'bigram' } });
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

test('runs only the selected stages and indexes the saved chunk ranges', { timeout: 30_000 }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-stages-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const run = (...args) => runNode(process.execPath, [cli, ...args], { cwd: root, encoding: 'utf8', timeout: 10_000 });
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
  const run = (cwd, ...args) => runNode(process.execPath, [cli, ...args], { cwd, timeout: 10_000, encoding: 'utf8' });
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
