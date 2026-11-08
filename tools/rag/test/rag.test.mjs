import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { promisify } from 'node:util';

const runNode = promisify(execFile);
const cli = fileURLToPath(new URL('../cli.mjs', import.meta.url));
const retention = 'バックアップの保管期間は30日です。';
const recovery = 'バックアップの復旧時間は4時間です。';

test('answers from ingested folders and S3 with verified citations through each provider', { timeout: 40_000 }, async (t) => {
  const requests = [];
  const server = createServer(async (request, response) => {
    const { pathname } = new URL(request.url, 'http://localhost');
    if (request.method === 'GET' && pathname === '/test-bucket/restore.md') {
      response.writeHead(200, { 'content-type': 'text/markdown' });
      response.end(`# Overview\nNotes\n## Recovery\r\n${recovery}\r\n`);
      return;
    }
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const payload = JSON.parse(Buffer.concat(chunks).toString('utf8'));
    const input = JSON.parse(payload.input ?? payload.messages[1].content);
    requests.push({ path: request.url, auth: request.headers.authorization, payload, input });
    const claims = [retention, recovery].flatMap((text) => {
      for (const item of input.evidence) {
        const line = item.lines.find((entry) => entry.text === text);
        if (line) return [{ text, citations: [{ id: item.id, startLine: line.number, endLine: line.number }] }];
      }
      return [];
    });
    if (payload.model === 'invalid') claims[0].citations[0].endLine = 999;
    const content = JSON.stringify({ claims });
    let result;
    if (request.url === '/v1/responses') {
      result = { status: 'completed', output: [{ type: 'message', content: [{ type: 'output_text', text: content }] }] };
    } else if (request.url === '/api/chat') {
      result = { done: true, done_reason: 'stop', message: { content } };
    } else {
      result = { choices: [{ finish_reason: 'stop', message: { content } }] };
    }
    response.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify(result));
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  t.after(() => new Promise((resolve) => server.close(resolve)));
  const origin = `http://127.0.0.1:${server.address().port}`;
  const root = await mkdtemp(join(tmpdir(), 'dunlin-rag-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const index = join(root, 'index');
  const folder = join(root, 'docs');
  const document = join(folder, 'backup.md');
  await mkdir(folder);
  await writeFile(document, `# Overview\nNotes\n## Retention\n${retention}\n`);
  const env = {
    ...process.env, OPENAI_API_KEY: 'fixture-openai', STRATA_API_KEY: 'fixture-strata',
    AWS_REGION: 'us-east-1', AWS_ACCESS_KEY_ID: 'fixture-access', AWS_SECRET_ACCESS_KEY: 'fixture-secret',
    AWS_SESSION_TOKEN: 'fixture-token', AWS_PROFILE: '', AWS_EC2_METADATA_DISABLED: 'true',
  };
  const run = (command, ...args) => runNode(process.execPath, [cli, command, '--index', index, ...args], { env, timeout: 10_000 });
  await run('ingest', '--source', 'local', folder);
  await run('ingest', '--source', 's3', '--s3-endpoint', origin, 's3://test-bucket/restore.md');

  const question = 'バックアップの保管期間と復旧時間はどれくらいですか？';
  const providers = [
    ['openai', `${origin}/v1`, '/v1/responses', 'Bearer fixture-openai'],
    ['ollama', origin, '/api/chat', undefined],
    ['strata', `${origin}/v1`, '/v1/chat/completions', 'Bearer fixture-strata'],
  ];
  for (const [provider, base, path, auth] of providers) {
    const { stdout } = await run('ask', '--provider', provider, '--model', 'fixture', '--base-url', base, '--json', question);
    const result = JSON.parse(stdout);
    assert.equal(result.status, 'answered');
    assert.equal(result.answer, `${retention} [1]\n${recovery} [2]`);
    assert.deepEqual(result.citations, [
      { id: 1, url: pathToFileURL(document).href, range: { startLine: 4, endLine: 4 }, content: `${retention}\n` },
      { id: 2, url: 's3://test-bucket/restore.md', range: { startLine: 4, endLine: 4 }, content: `${recovery}\r\n` },
    ]);
    const request = requests.at(-1);
    assert.equal(request.path, path);
    assert.equal(request.auth, auth);
    assert.equal(request.input.question, question);
    assert.ok(request.input.evidence.every((excerpt) => !('url' in excerpt)));
  }
  assert.equal(requests[0].payload.store, false);
  assert.equal(requests[0].payload.text.format.type, 'json_schema');
  assert.equal(requests[1].payload.format.type, 'object');
  assert.equal(requests[2].payload.response_format.type, 'json_schema');

  const scoped = await run('ask', '--provider', 'ollama', '--model', 'fixture', '--base-url', origin, '--source', 'local', question);
  assert.match(scoped.stdout, /30日.*\[1\]/u);
  assert.match(scoped.stdout, /\(4–4\)/u);
  assert.ok(!scoped.stdout.includes('s3://'));

  const requestsBefore = requests.length;
  const missing = await run('ask', '--provider', 'ollama', '--model', 'fixture', '--base-url', origin, '--json', 'unfindable');
  assert.equal(JSON.parse(missing.stdout).status, 'insufficient_evidence');
  assert.equal(requests.length, requestsBefore);

  await assert.rejects(
    run('ask', '--provider', 'ollama', '--model', 'invalid', '--base-url', origin, question),
    /outside the retrieved evidence/u,
  );
});
