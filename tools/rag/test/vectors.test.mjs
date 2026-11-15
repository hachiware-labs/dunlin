import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

import { Dunlin } from 'dunlin';

const runNode = promisify(execFile);
const cli = fileURLToPath(new URL('./fixtures/legacy-cli.mjs', import.meta.url));
const sourceUrl = 's3://test-bucket/retention.md';
const fact = 'Backup snapshots expire after 37 days.';
const question = 'Preservation lifespan?';

test('builds and searches independent embedding indexes with real line evidence', { timeout: 30_000 }, async (t) => {
  const requests = [];
  let invalidVectors = false;
  const server = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const payload = JSON.parse(Buffer.concat(chunks).toString('utf8'));
    requests.push({ path: request.url, auth: request.headers.authorization, payload });
    let result;
    if (request.url === '/api/chat') {
      const { evidence } = JSON.parse(payload.messages[1].content);
      const item = evidence[0];
      const line = item.lines.find((entry) => entry.text === fact);
      const claims = line ? [{ text: fact, citations: [{ id: item.id, startLine: line.number, endLine: line.number }] }] : [];
      result = { done: true, done_reason: 'stop', message: { content: JSON.stringify({ claims }) } };
    } else {
      const vectors = payload.input.map((text) => {
        if (invalidVectors) return [0, 0];
        return text === question || text.includes(fact) ? [2, 0] : [0, 3];
      });
      result = request.url === '/v1/embeddings'
        ? { data: vectors.map((embedding, index) => ({ index, embedding })).reverse() }
        : { embeddings: vectors };
    }
    response.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify(result));
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  t.after(() => new Promise((resolve) => server.close(resolve)));
  const origin = `http://127.0.0.1:${server.address().port}`;
  const root = await mkdtemp(join(tmpdir(), 'dunlin-rag-vectors-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const index = join(root, 'index');
  const folder = join(root, 'docs');
  await mkdir(folder);
  await writeFile(join(folder, 'gardening.txt'), 'Greenhouse filters require monthly cleaning.');
  const env = { ...process.env, OPENAI_API_KEY: 'fixture-embedding-key' };
  const run = (command, ...args) => runNode(process.execPath, [cli, command, '--index', index, ...args], { env, timeout: 10_000 });
  await run('ingest', '--source', 'local', folder);

  async function replaceRemote(content) {
    const database = await Dunlin.open(index);
    try {
      await database.replaceDocuments('remote', [{ objectId: 'retention.md', url: sourceUrl, format: 'markdown', content }]);
    } finally {
      await database.close();
    }
  }
  await replaceRemote(`# Policy\r\n${fact}\r\n`);

  const ask = (...extra) => run('ask', '--provider', 'ollama', '--model', 'fixture-generator', '--base-url', origin, '--json', ...extra, question);
  const keyword = await ask();
  assert.equal(JSON.parse(keyword.stdout).status, 'insufficient_evidence');
  assert.equal(requests.length, 0);

  const configs = [
    ['ollama', origin, '/api/embed', undefined],
    ['openai', `${origin}/v1`, '/v1/embeddings', 'Bearer fixture-embedding-key'],
  ];
  let flags;
  for (const [provider, base, path, auth] of configs) {
    flags = ['--embedding-provider', provider, '--embedding-model', 'fixture-embedder', '--embedding-base-url', base];
    const { stdout: stats } = await run('embed', ...flags);
    assert.deepEqual(JSON.parse(stats), { documents: 2, chunks: 2, dimensions: 2 });
    const build = requests.at(-1);
    assert.equal(build.path, path);
    assert.equal(build.auth, auth);
    assert.equal(build.payload.model, 'fixture-embedder');
    assert.equal(build.payload.input.length, 2);
    const { stdout } = await ask(...flags);
    assert.equal(requests.at(-2).path, path);
    assert.deepEqual(requests.at(-2).payload.input, [question]);
    assert.deepEqual(JSON.parse(stdout), {
      status: 'answered', answer: `${fact} [1]`, citations: [
        { id: 1, url: sourceUrl, range: { startLine: 2, endLine: 2 }, content: `${fact}\r\n` },
      ],
    });
    const scoped = await ask(...flags, '--source', 'local');
    assert.equal(JSON.parse(scoped.stdout).status, 'insufficient_evidence');
  }
  invalidVectors = true;
  await assert.rejects(run('embed', ...flags), /invalid vector norm/u);
  invalidVectors = false;
  assert.equal(JSON.parse((await ask(...flags)).stdout).status, 'answered');

  await replaceRemote('# Updated\nA new policy applies.');
  const before = requests.length;
  await assert.rejects(ask(...flags), /Vector index is stale/u);
  assert.equal(requests.length, before);
});
