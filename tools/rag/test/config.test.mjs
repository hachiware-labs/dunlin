import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { once } from 'node:events';
import { access, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { Readable, Writable } from 'node:stream';
import { test } from 'node:test';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { promisify } from 'node:util';

import { Dunlin } from 'dunlin';
import { promptInitPreset } from '../init-prompt.mjs';

const runNode = promisify(execFile);
const cli = fileURLToPath(new URL('./fixtures/legacy-cli.mjs', import.meta.url));

test('asks for a preset and accepts the default confirmation or cancellation', async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-init-prompt-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  async function prompt(answers) {
    let transcript = '';
    const output = new Writable({ write(chunk, _encoding, callback) {
      transcript += chunk.toString('utf8');
      callback();
    } });
    const preset = await promptInitPreset(root, { input: Readable.from([answers]), output });
    return { preset, transcript };
  }

  const { preset, transcript } = await prompt('9\n2\n\n');
  assert.equal(preset, 'llm-wiki');
  assert.ok(transcript.includes(root));
  assert.match(transcript, /1〜4 の番号を入力してください/u);
  assert.match(transcript, /\[Y\/n\]/u);
  assert.equal((await prompt('1\nn\n')).preset, null);
  assert.equal((await prompt('4\n')).preset, null);
  await assert.rejects(access(join(root, 'dunlin.json')), { code: 'ENOENT' });

  const { stdout, stderr } = await runNode(process.execPath, [cli, 'init'], { cwd: root, timeout: 10_000 });
  assert.equal(stdout.trim(), join(root, 'dunlin.json'));
  assert.match(stderr, /次の手順（RAG）/u);
  assert.doesNotMatch(stderr, /用途を選んでください/u);
});

test('initializes the default input folder and prints guidance for each preset', async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-init-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  for (const [preset, next] of [['rag', /dunlin ask/u], ['mcp', /knowledge\.search/u],
    ['para', /para\.save/u], ['llm-wiki', /wiki\.apply/u]]) {
    const project = join(root, `${preset} project`);
    const { stdout, stderr } = await runNode(process.execPath, [cli, 'init', project, '--preset', preset]);
    assert.equal(stdout.trim(), join(project, 'dunlin.json'));
    assert.match(stderr, next);
    assert.ok(stderr.includes(project));
    await access(join(project, 'docs'));
  }
});

test('keeps corpus information with the shared corpus across ingestion and configuration files', async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-corpus-info-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const run = (...args) => runNode(process.execPath, [cli, ...args], { cwd: root });
  const show = async (...args) => JSON.parse((await run('corpus', 'show', ...args)).stdout);
  const manifest = join(root, 'knowledge', '.dunlin-corpus-v1', 'manifest.json');

  await run('init', '.', '--preset', 'mcp');
  const initial = await show();
  assert.equal(initial.version, 1);
  assert.equal(initial.status.corpusBackend, 'sqlite');
  assert.deepEqual(initial.status.sources, [{ sourceId: 'docs', configured: true, documents: 0 }]);
  assert.equal(initial.status.search.keyword.state, 'not_indexed');
  await assert.rejects(access(join(root, 'knowledge')), { code: 'ENOENT' });
  await run('corpus', 'set', 'name', 'バックアップ運用');
  await run('corpus', 'set', 'description', '保管方針と復旧手順');
  await run('corpus', 'set', 'metadata.team', 'インフラ運用');
  await run('corpus', 'set', 'metadata.tags', '["運用","規程"]');
  const expected = { version: 1, name: 'バックアップ運用', description: '保管方針と復旧手順',
    metadata: { team: 'インフラ運用', tags: ['運用', '規程'] } };

  await run('config', 'set', 'corpus.backend', 'json');
  assert.equal((await show()).status.corpusBackend, 'json');
  await run('ingest');
  assert.equal((await show()).status.search.keyword.state, 'empty');
  await writeFile(join(root, 'docs', 'policy.md'), '# バックアップ\n30日保管する。\n');
  await run('ingest');
  const sqlite = join(root, 'knowledge', '.dunlin-corpus-v1', 'corpus.sqlite');
  await assert.rejects(access(sqlite), { code: 'ENOENT' });
  await run('config', 'set', 'corpus.backend', 'sqlite');
  await run('ingest', '--source', 'extra', './docs/policy.md');
  await access(sqlite);
  await mkdir(join(root, 'shared'));
  const shared = join(root, 'shared', 'dunlin.json');
  await writeFile(shared, JSON.stringify({ version: 1, index: '../knowledge' }));
  const { status: sharedStatus, ...sharedInfo } = await show('--config', shared);
  assert.deepEqual(sharedInfo, expected);
  assert.equal(sharedStatus.configPath, shared);
  assert.ok(sharedStatus.sources.every(({ configured }) => !configured));
  const { status, ...info } = await show('--index', './knowledge');
  assert.deepEqual(info, expected);
  assert.equal(status.configPath, join(root, 'dunlin.json'));
  assert.equal(status.indexPath, join(root, 'knowledge'));
  assert.deepEqual(status.sources, [
    { sourceId: 'docs', configured: true, documents: 1 },
    { sourceId: 'extra', configured: false, documents: 1 },
  ]);
  assert.deepEqual(status.search, { keyword: { state: 'ready', searchableDocuments: 2 }, vector: { state: 'not_configured' } });
  await assert.rejects(run('corpus', 'set', 'name', ' '), /Expected name to be nonempty/u);
  assert.deepEqual(JSON.parse(await readFile(manifest, 'utf8')), expected);
});

test('initializes a project and applies configuration edits to the next ingest, embed and ask', { timeout: 40_000 }, async (t) => {
  const requests = [];
  const server = createServer(async (request, response) => {
    const { pathname } = new URL(request.url, 'http://localhost');
    if (request.method === 'GET' && pathname === '/test-bucket/policy.md') {
      requests.push({ pathname });
      response.writeHead(200, { 'content-type': 'text/markdown' }).end('Remote backup policy');
      return;
    }
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const payload = JSON.parse(Buffer.concat(chunks).toString('utf8'));
    requests.push({ pathname, model: payload.model, auth: request.headers.authorization });
    let result;
    if (pathname === '/api/embed') {
      result = { embeddings: payload.input.map(() => [1, 0]) };
    } else {
      const { evidence } = JSON.parse(payload.messages[1].content);
      const excerpt = evidence.find(({ lines }) => lines.some(({ text }) => text.includes('Backup retention')));
      const line = excerpt.lines.find(({ text }) => text.includes('Backup retention'));
      const claims = [{ text: line.text, citations: [{ id: excerpt.id, startLine: line.number, endLine: line.number }] }];
      result = { done: true, done_reason: 'stop', message: { content: JSON.stringify({ claims }) } };
    }
    response.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify(result));
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  t.after(() => new Promise((resolve) => server.close(resolve)));
  const origin = `http://127.0.0.1:${server.address().port}`;
  const root = await mkdtemp(join(tmpdir(), 'dunlin-config-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const project = join(root, 'project');
  const configFile = join(project, 'dunlin.json');
  const nested = join(project, 'nested');
  const docs = join(project, 'docs');
  const second = join(project, 'docs-updated');
  const firstFile = join(docs, 'policy.md');
  const secondFile = join(second, 'policy.md');
  const env = {
    ...process.env, AWS_REGION: 'us-east-1', AWS_ACCESS_KEY_ID: 'fixture-access', AWS_SECRET_ACCESS_KEY: 'fixture-secret',
    AWS_SESSION_TOKEN: 'fixture-token', AWS_PROFILE: '', AWS_EC2_METADATA_DISABLED: 'true',
    DUNLIN_TEST_INDEX: 'custom-index', DUNLIN_TEST_DOCS: 'docs', DUNLIN_TEST_PROVIDER: 'ollama',
    DUNLIN_TEST_GENERATOR: 'generator-v1', DUNLIN_TEST_EMBEDDER: 'embedding-v1', DUNLIN_TEST_ORIGIN: origin,
    DUNLIN_TEST_STORE: 'local', DUNLIN_TEST_BUCKET: 'test-bucket',
    DUNLIN_TEST_GENERATOR_KEY: 'fixture-generator-key', DUNLIN_TEST_EMBEDDER_KEY: 'fixture-embedder-key',
    DUNLIN_TEST_UNSET: undefined,
  };
  const run = (cwd, ...args) => runNode(process.execPath, [cli, ...args], { cwd, env, timeout: 10_000 });
  await run(root, 'init', './project', '--preset', 'rag');
  await Promise.all([mkdir(nested), mkdir(second)]);
  await Promise.all([
    writeFile(firstFile, '# Policy\nBackup retention is 30 days.\n'),
    writeFile(secondFile, '# Policy\nBackup retention is 45 days.\n'),
  ]);
  const original = await readFile(configFile, 'utf8');
  assert.equal(JSON.parse(original).rag.vectorStore.type, 'sqlite');
  await assert.rejects(run(root, 'init', './project'), /Configuration already exists/u);
  assert.equal(await readFile(configFile, 'utf8'), original);

  const inProject = (...args) => run(nested, ...args);
  const set = (key, value) => inProject('config', 'set', key, typeof value === 'string' ? value : JSON.stringify(value));
  const show = async () => JSON.parse((await inProject('config', 'show')).stdout);
  const searchStatus = async () => JSON.parse((await inProject('corpus', 'show')).stdout).status.search;
  const ask = async (...args) => JSON.parse((await inProject('ask', '--source', 'docs', '--json', ...args, 'What is the backup retention?')).stdout);

  await set('index', './${DUNLIN_TEST_INDEX}');
  await set('sources.docs', './${DUNLIN_TEST_DOCS}');
  await set('rag.generation', {
    provider: '${DUNLIN_TEST_PROVIDER}', model: '${DUNLIN_TEST_GENERATOR}', baseUrl: '${DUNLIN_TEST_UNSET}',
    apiKeyEnv: 'DUNLIN_TEST_GENERATOR_KEY',
  });
  await set('rag.embedding', {
    provider: 'ollama', model: '${DUNLIN_TEST_EMBEDDER}', baseUrl: '${DUNLIN_TEST_ORIGIN}',
    apiKeyEnv: 'DUNLIN_TEST_EMBEDDER_KEY',
  });
  await set('rag.vectorStore.type', '${DUNLIN_TEST_STORE}');
  await set('sources.remote.v1', 's3://${DUNLIN_TEST_BUCKET}/policy.md');
  await set('keepOriginals.docs', true);
  await set('s3Endpoint', '${DUNLIN_TEST_ORIGIN}');
  assert.deepEqual(await show(), JSON.parse((await run(root, 'config', 'show', '--config', './project/dunlin.json')).stdout));
  assert.equal((await show()).rag.generation.baseUrl, '${DUNLIN_TEST_UNSET}');

  const beforeInvalid = await readFile(configFile, 'utf8');
  await assert.rejects(set('rag.vectorStore.type', 'unknown'), /Expected rag.vectorStore.type/u);
  await assert.rejects(set('index', './${INVALID-NAME}'), /Invalid environment reference in index/u);
  await assert.rejects(set('rag.generation', { provider: 'ollama' }), /Expected rag.generation.model/u);
  assert.equal(await readFile(configFile, 'utf8'), beforeInvalid);
  assert.ok(!beforeInvalid.includes('fixture-generator-key'));
  assert.ok(!beforeInvalid.includes('fixture-embedder-key'));

  const { stdout: ingested } = await inProject('ingest');
  assert.deepEqual(JSON.parse(ingested).map(({ sourceId, files }) => [sourceId, files]), [['docs', 1], ['remote.v1', 1]]);
  assert.ok(requests.some(({ pathname }) => pathname === '/test-bucket/policy.md'));
  await access(join(project, 'custom-index', '.dunlin-index-v1'));
  const database = await Dunlin.open(join(project, 'custom-index'));
  try {
    for (const [sourceId, objectId, keepsOriginal] of [
      ['docs', 'policy.md', true], ['remote.v1', 's3://test-bucket/policy.md', false],
    ]) {
      const ref = { sourceId, objectId };
      const indexed = await database.getDocument(ref);
      const saved = await database.getCorpusDocument(ref, indexed.evidence.source.version);
      assert.equal(saved.content, indexed.content);
      assert.equal(Boolean(saved.originalSha256), keepsOriginal);
    }
  } finally {
    await database.close();
  }
  assert.equal((await searchStatus()).vector.state, 'missing');
  const { stdout: embedded } = await run(root, 'embed', '--config', './project/dunlin.json');
  assert.equal(JSON.parse(embedded).documents, 2);
  assert.equal(requests.at(-1).model, 'embedding-v1');
  assert.equal(requests.at(-1).auth, 'Bearer fixture-embedder-key');
  const beforeStatus = requests.length;
  assert.deepEqual((await searchStatus()).vector, { state: 'ready', chunks: 2 });
  assert.equal(requests.length, beforeStatus);
  const beforeMissing = requests.length;
  await assert.rejects(ask(), /DUNLIN_TEST_UNSET is required by rag\.generation\.baseUrl/u);
  assert.equal(requests.length, beforeMissing);
  const initial = await ask('--base-url', origin);
  assert.equal(initial.citations[0].url, pathToFileURL(firstFile).href);
  assert.match(initial.answer, /30 days/u);
  assert.equal(requests.at(-1).model, 'generator-v1');
  assert.equal(requests.at(-1).auth, 'Bearer fixture-generator-key');

  await set('rag.generation.baseUrl', '${DUNLIN_TEST_ORIGIN}');
  await set('rag.generation.apiKeyEnv', 'DUNLIN_TEST_UNSET');
  await assert.rejects(ask(), /DUNLIN_TEST_UNSET is required by rag\.generation\.apiKeyEnv/u);
  await set('rag.generation.apiKeyEnv', 'DUNLIN_TEST_GENERATOR_KEY');
  env.DUNLIN_TEST_GENERATOR_KEY = 'fixture-invalid-key\nextra';
  await assert.rejects(ask(), (error) => /contains an invalid API key/u.test(error.stderr)
    && !error.stderr.includes('fixture-invalid-key'));
  env.DUNLIN_TEST_GENERATOR_KEY = 'fixture-generator-key';

  env.DUNLIN_TEST_GENERATOR = 'generator-via-env';
  await ask();
  assert.equal(requests.at(-1).model, 'generator-via-env');
  assert.equal((await show()).rag.generation.model, '${DUNLIN_TEST_GENERATOR}');

  await set('rag.generation.model', 'generator-v2');
  assert.match((await ask()).answer, /30 days/u);
  assert.equal(requests.at(-1).model, 'generator-v2');
  await ask('--model', 'temporary-override');
  assert.equal(requests.at(-1).model, 'temporary-override');
  assert.equal((await show()).rag.generation.model, 'generator-v2');

  await set('rag.embedding.model', 'embedding-v2');
  await assert.rejects(ask(), /Vector index missing/u);
  await inProject('embed');
  assert.equal(requests.at(-1).model, 'embedding-v2');
  await set('rag.vectorStore.type', 'sqlite');
  await assert.rejects(ask(), /Vector index missing/u);
  await inProject('embed');
  assert.equal((await ask()).status, 'answered');

  await set('sources.docs', './docs-updated');
  await inProject('ingest', '--source', 'docs');
  assert.equal((await searchStatus()).vector.state, 'stale');
  await assert.rejects(ask(), /Vector index is stale/u);
  await inProject('embed');
  const updated = await ask();
  assert.match(updated.answer, /45 days/u);
  assert.equal(updated.citations[0].url, pathToFileURL(secondFile).href);
  assert.deepEqual(updated.citations[0].range, { startLine: 2, endLine: 2 });

  await set('rag.embedding', null);
  const beforeKeyword = requests.length;
  assert.match((await ask()).answer, /45 days/u);
  assert.deepEqual(requests.slice(beforeKeyword).map(({ pathname }) => pathname), ['/api/chat']);

  await run(root, 'init', './mcp', '--preset', 'mcp');
  assert.deepEqual(JSON.parse(await readFile(join(root, 'mcp', 'dunlin.json'), 'utf8')), {
    version: 1, index: './knowledge', corpus: { backend: 'sqlite' }, sources: { docs: './docs' },
  });
});
