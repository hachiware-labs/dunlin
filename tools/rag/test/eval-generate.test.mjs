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
  const root = await mkdtemp(join(tmpdir(), 'knowledge-nest-generate-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const requests = [];
  const prompts = [];
  const one = (question, doc, quote, kind = 'lexical') => ({ question, kind, evidence: [{ doc, quote }] });
  // The first line of an excerpt that is short enough to quote.
  const line = (text) => text.split('\n').find((value) => value && value.length <= 300);
  let reply = (refs) => ({ cases: [
    one('存在しない引用', refs[0].doc, '資料にない文'),
    one('繰り返しの語', 'docs:repeat.txt', '点検'),
    one(refs[0].doc + ' の内容は', refs[0].doc, line(refs[0].text)),
    ...refs.map(({ doc, text }) => one(doc + ' の内容は', doc, line(text))),
  ] });
  const server = createServer(async (request, response) => {
    const body = [];
    for await (const chunk of request) body.push(chunk);
    const { messages } = JSON.parse(Buffer.concat(body));
    const refs = JSON.parse(messages[1].content);
    requests.push(refs);
    prompts.push(messages[0].content);
    response.setHeader('content-type', 'application/json');
    const content = reply(refs);
    response.end(JSON.stringify({ done: true, message: { content: typeof content === 'string' ? content : JSON.stringify(content) } }));
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  t.after(() => new Promise((resolve) => server.close(resolve)));
  const env = { ...process.env, KNOWLEDGE_NEST_GLOBAL_CONFIG: join(root, 'global.yaml') };
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
  const configPath = join(root, 'kb', 'KnowledgeNestfile.yaml');
  const config = parse(await readFile(configPath, 'utf8'));
  config.models = { writer: { provider: 'ollama', kind: 'chat', model: 'testing', base_url: 'http://127.0.0.1:' + server.address().port } };
  await writeFile(configPath, stringify(config));
  await json('build');
  await json('eval-data', 'add', '--question', '交換周期', '--doc', 'covered.txt', '--quote', 'フィルターは6か月ごとに交換する。');

  await rejects(json('eval-data', 'generate', '--count', '0'), 'INVALID_EVAL_DATA');
  const result = await json('eval-data', 'generate', '--count', '4');
  assert.equal(result.generated, 4);
  assert.equal(result.cases, 5);
  assert.deepEqual(result.kinds, { lexical: 4, paraphrase: 0, identifier: 0, variant: 0 });
  assert.deepEqual(result.dropped, { quote_not_in_excerpt: 1, duplicate: 1, quote_not_unique: 1 });
  assert.match(prompts[0], /Use the four kinds about equally\. .* at most 300 characters /u);
  // Documents without cases are offered before the one that already has a case.
  assert.deepEqual(requests[0].map(({ doc }) => doc),
    ['docs:long.txt', 'docs:pump.txt', 'docs:repeat.txt', 'docs:covered.txt', 'docs:long.txt']);
  assert.ok(result.documents.includes('docs:long.txt'));
  const { cases } = await json('eval-data', 'show', result.id);
  const generated = cases.filter(({ state }) => state === 'generated');
  assert.equal(generated.length, 4);
  assert.ok(generated.every(({ input_by, generation, kind }) => input_by === 'model' && generation.model === 'writer'
    && generation.prompt_version === 4 && kind === 'lexical'));

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
  reply = (refs) => ({ cases: refs.filter(({ text }) => text.includes('後半')).map(({ doc }) => one('後半で読むものは', doc, '後半の注意事項を読む。')) });
  const more = await json('eval-data', 'generate', '--model', 'writer', '--doc', 'long.txt', '--count', '1');
  assert.equal(more.generated, 1);
  assert.deepEqual(requests.map((refs) => refs.length), [2]);
  // The first part has the quote of a case, so the part without one is offered first.
  assert.ok(requests[0][0].text.includes('後半') && requests[0][1].text.startsWith('前半'));

  // --kind keeps the candidates of one kind and says so to the model.
  const pump = 'ポンプの点検は毎月最初の営業日に行う。';
  reply = () => ({ cases: [one('ポンプの点検はいつ', 'docs:pump.txt', pump), one('月初の作業は', 'docs:pump.txt', pump, 'paraphrase')] });
  const paraphrases = await json('eval-data', 'generate', '--doc', 'pump.txt', '--kind', 'paraphrase', '--count', '2');
  assert.deepEqual(paraphrases.kinds, { lexical: 0, paraphrase: 1, identifier: 0, variant: 0 });
  assert.deepEqual(paraphrases.dropped, { kind_not_requested: 1 });
  assert.match(prompts.at(-1), /Write only paraphrase questions\. /u);
  await rejects(json('eval-data', 'generate', '--kind', 'other'), 'INVALID_EVAL_DATA');

  // A question may need quotes from several documents. A quote given twice counts once.
  reply = () => ({ cases: [{ question: '点検について決まっていること', kind: 'paraphrase', evidence: [
    { doc: 'docs:pump.txt', quote: pump }, { doc: 'docs:repeat.txt', quote: '点検の責任者を決める。' }, { doc: 'docs:pump.txt', quote: pump }] }] });
  const several = await json('eval-data', 'generate', '--count', '1');
  assert.deepEqual(several.documents, ['docs:pump.txt', 'docs:repeat.txt']);
  const added = (await json('eval-data', 'show', several.id)).cases.find(({ question }) => question === '点検について決まっていること');
  assert.deepEqual(added.evidence.map(({ doc, quote }) => [doc, quote]), [['docs:pump.txt', pump], ['docs:repeat.txt', '点検の責任者を決める。']]);
  assert.equal(added.kind, 'paraphrase');
  // One quote that is not in the excerpts drops the whole candidate.
  reply = () => ({ cases: [{ question: '一部が架空', kind: 'lexical', evidence: [{ doc: 'docs:pump.txt', quote: pump }, { doc: 'docs:repeat.txt', quote: '資料にない文' }] }] });
  await rejects(json('eval-data', 'generate', '--count', '1'), 'NO_VALID_CANDIDATES');

  const failure = (promise) => promise.then(() => assert.fail('generate succeeded'), (error) => JSON.parse(error.stdout).errors[0]);
  reply = () => ({ cases: [one('長い引用', 'docs:long.txt', 'あ'.repeat(301))] });
  const long = await failure(json('eval-data', 'generate', '--doc', 'long.txt'));
  assert.equal(long.code, 'NO_VALID_CANDIDATES');
  assert.match(long.message, /"quote_too_long":1/u);
  // A candidate of the earlier shape, or of an unknown kind, is not read.
  reply = (refs) => ({ cases: [{ question: '以前の形', doc: refs[0].doc, quote: pump }] });
  await rejects(json('eval-data', 'generate', '--doc', 'pump.txt'), 'INVALID_MODEL_OUTPUT');
  reply = () => ({ cases: [one('知らない種類', 'docs:pump.txt', pump, 'other')] });
  await rejects(json('eval-data', 'generate', '--doc', 'pump.txt'), 'INVALID_MODEL_OUTPUT');

  reply = () => 'not json';
  await rejects(json('eval-data', 'generate', '--doc', 'pump.txt'), 'INVALID_MODEL_OUTPUT');
  reply = (refs) => ({ cases: [one('架空', refs[0].doc, '資料にない文')] });
  await rejects(json('eval-data', 'generate', '--doc', 'pump.txt'), 'NO_VALID_CANDIDATES');
});
