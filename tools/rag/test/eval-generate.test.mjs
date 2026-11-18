import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

import { parse, stringify } from 'yaml';

const cli = fileURLToPath(new URL('../cli.mjs', import.meta.url));
const runNode = promisify(execFile);

test('generates evaluation candidates across uncovered documents and keeps only verified quotes', { timeout: 60_000 }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-generate-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const requests = [];
  let reply = (refs) => ({ cases: [
    { question: '存在しない引用', doc: refs[0].doc, quote: '資料にない文' },
    { question: '繰り返しの語', doc: 'docs:repeat.txt', quote: '点検' },
    { question: refs[0].doc + ' の内容は', doc: refs[0].doc, quote: refs[0].text.split('\n')[0] },
    ...refs.map(({ doc, text }) => ({ question: doc + ' の内容は', doc, quote: text.split('\n')[0] })),
  ] });
  const server = createServer(async (request, response) => {
    const body = [];
    for await (const chunk of request) body.push(chunk);
    const refs = JSON.parse(JSON.parse(Buffer.concat(body)).messages[1].content);
    requests.push(refs);
    response.setHeader('content-type', 'application/json');
    const content = reply(refs);
    response.end(JSON.stringify({ done: true, message: { content: typeof content === 'string' ? content : JSON.stringify(content) } }));
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  t.after(() => new Promise((resolve) => server.close(resolve)));
  const env = { ...process.env, DUNLIN_GLOBAL_CONFIG: join(root, 'global.yaml') };
  const run = (...args) => runNode(process.execPath, [cli, ...args], { cwd: root, env, encoding: 'utf8', timeout: 20_000 });
  const json = async (...args) => JSON.parse((await run('--project', 'kb', ...args, '--json')).stdout).data;
  const rejects = (promise, code) => assert.rejects(promise, (error) => JSON.parse(error.stdout).errors[0].code === code);

  await run('init', 'mcp', 'kb');
  const docs = join(root, 'kb', 'docs');
  await Promise.all([
    writeFile(join(docs, 'covered.txt'), 'フィルターは6か月ごとに交換する。\n'),
    writeFile(join(docs, 'pump.txt'), 'ポンプの点検は毎月最初の営業日に行う。\n'),
    writeFile(join(docs, 'repeat.txt'), '点検の記録を残す。点検の責任者を決める。\n'),
    writeFile(join(docs, 'long.txt'), '前半の手順を確認する。\n' + 'あ'.repeat(4500) + '\n後半の注意事項を読む。\n'),
  ]);
  const configPath = join(root, 'kb', 'Dunlinfile.yaml');
  const config = parse(await readFile(configPath, 'utf8'));
  config.models = { writer: { provider: 'ollama', kind: 'chat', model: 'testing', base_url: 'http://127.0.0.1:' + server.address().port } };
  await writeFile(configPath, stringify(config));
  await json('build');
  await json('eval-data', 'add', '--question', '交換周期', '--doc', 'covered.txt', '--quote', 'フィルターは6か月ごとに交換する。');

  await rejects(json('eval-data', 'generate', '--count', '0'), 'INVALID_EVAL_DATA');
  const result = await json('eval-data', 'generate', '--count', '4');
  assert.equal(result.generated, 4);
  assert.equal(result.cases, 5);
  assert.deepEqual(result.dropped, { quote_not_in_excerpt: 1, duplicate: 1, quote_not_unique: 1 });
  // Documents without cases are offered before the one that already has a case.
  assert.deepEqual(requests[0].map(({ doc }) => doc),
    ['docs:long.txt', 'docs:pump.txt', 'docs:repeat.txt', 'docs:covered.txt', 'docs:long.txt']);
  assert.ok(result.documents.includes('docs:long.txt'));
  const { cases } = await json('eval-data', 'show', result.id);
  const generated = cases.filter(({ state }) => state === 'generated');
  assert.equal(generated.length, 4);
  assert.ok(generated.every(({ input_by, generation }) => input_by === 'model' && generation.model === 'writer' && generation.prompt_version === 2));

  // Generated candidates stay out of the scores until they are reviewed.
  const before = await json('eval', '--tag', 'before');
  assert.equal(before.rows.length, 1);
  const pending = await json('eval-data', 'review', result.id);
  assert.equal(pending.cases.length, 4);
  const [first, second, third] = pending.cases.map(({ id }) => id);
  const reviewed = await json('eval-data', 'review', result.id, '--case', first, '--case', second,
    '--state', 'reviewed', '--reviewer', 'tester', '--reviewer-type', 'human');
  assert.deepEqual(reviewed.cases.map(({ state }) => state), ['reviewed', 'reviewed']);
  await json('eval-data', 'review', reviewed.id, '--case', third, '--state', 'rejected', '--reviewer', 'tester', '--reviewer-type', 'agent');
  assert.equal((await json('eval', '--tag', 'after')).rows.length, 3);

  // The part of a long document beyond the first excerpt is also offered.
  requests.length = 0;
  reply = (refs) => ({ cases: refs.filter(({ text }) => text.includes('後半')).map(({ doc }) => ({ question: '後半で読むものは', doc, quote: '後半の注意事項を読む。' })) });
  const more = await json('eval-data', 'generate', '--model', 'writer', '--doc', 'long.txt', '--count', '1');
  assert.equal(more.generated, 1);
  assert.deepEqual(requests.map((refs) => refs.length), [2]);

  reply = () => 'not json';
  await rejects(json('eval-data', 'generate', '--doc', 'pump.txt'), 'INVALID_MODEL_OUTPUT');
  reply = (refs) => ({ cases: [{ question: '架空', doc: refs[0].doc, quote: '資料にない文' }] });
  await rejects(json('eval-data', 'generate', '--doc', 'pump.txt'), 'NO_VALID_CANDIDATES');
});
