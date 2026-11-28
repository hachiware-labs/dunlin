// nest ask sends the model the passages alone, and keeps only the statements the passages it
// cites bear out: not the question said again, not numbers the passages do not have, and not what
// the question asks about put onto passages about something else. With none left, it declines.

import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { parse, stringify } from 'yaml';

const cli = fileURLToPath(new URL('../cli.mjs', import.meta.url));
const runNode = promisify(execFile);

test('keeps the statements the passages bear out, and declines when none are left', async (t) => {
  const requests = [];
  // The test model answers every question with the question again, a number the passage does not
  // have, and what the question is about with the day the passage gives ("ポンプの点検は…" to a
  // question about the pump, "有給休暇は…" to one about leave), unless it is told not to.
  let honest = true;
  const server = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const body = JSON.parse(Buffer.concat(chunks).toString('utf8'));
    requests.push(body);
    const { question } = JSON.parse(body.messages[1].content);
    const subject = question.split('は')[0];
    const facts = [
      { text: question, evidence_ids: [1] },
      { text: 'ポンプの点検は毎月10日に行う。', evidence_ids: [1] },
      ...(honest ? [{ text: `${subject}は毎月最初の営業日に行う。`, evidence_ids: [1] }] : []),
    ];
    response.setHeader('content-type', 'application/json');
    response.end(JSON.stringify({ done: true, prompt_eval_count: 10, eval_count: 5, message: { content: JSON.stringify({ facts }) } }));
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const root = await mkdtemp(join(tmpdir(), 'knowledge-nest-ask-checks-'));
  t.after(async () => {
    server.close();
    await rm(root, { recursive: true, force: true, maxRetries: 5 });
  });
  const env = { ...process.env, KNOWLEDGE_NEST_GLOBAL_CONFIG: join(root, 'global.yaml') };
  const run = (...args) => runNode(process.execPath, [cli, '--project', 'kb', ...args], { cwd: root, env, encoding: 'utf8', timeout: 30_000 });
  const ask = async (question) => JSON.parse((await run('ask', question, '--json')).stdout).data;
  await runNode(process.execPath, [cli, 'init', 'rag', 'kb', '--llm-provider', 'ollama', '--llm-model', 'test', '--no-embedding', '--no-ocr'], { cwd: root, env });
  const configFile = join(root, 'kb', 'KnowledgeNestfile.yaml');
  const config = parse(await readFile(configFile, 'utf8'));
  config.models.answerer.base_url = `http://127.0.0.1:${server.address().port}`;
  await writeFile(configFile, stringify(config));
  await writeFile(join(root, 'kb', 'docs', 'pump.md'), '# ポンプの保全\r\n\r\nポンプの点検は毎月最初の営業日に行う。有給の作業員が行う。\r\n');
  await run('build');

  const answered = await ask('ポンプの点検はいつ？');
  // The model is sent the question and the passages alone, with room to think before it answers.
  const sent = JSON.parse(requests[0].messages[1].content);
  assert.deepEqual(Object.keys(sent), ['question', 'evidence']);
  assert.deepEqual(sent.evidence, [{ id: 1, document: 'docs:pump.md', text: '# ポンプの保全\n\nポンプの点検は毎月最初の営業日に行う。有給の作業員が行う。\n' }]);
  assert.equal(requests[0].options.num_predict, 4096);
  assert.match(requests[0].messages[0].content, /Do not repeat the question/u);
  // Only the statement the passage bears out is the answer.
  assert.equal(answered.output[0].content[0].text, 'ポンプの点検は毎月最初の営業日に行う。');
  assert.equal(answered.knowledge_nest_citations.length, 1);
  assert.deepEqual(answered.knowledge_nest_rejected, [
    { text: 'ポンプの点検はいつ？', reason: 'restates the question' },
    { text: 'ポンプの点検は毎月10日に行う。', reason: 'has numbers or quotations that the passages it cites do not' },
  ]);
  const text = (await run('ask', 'ポンプの点検はいつ？')).stdout;
  assert.match(text, /^ポンプの点検は毎月最初の営業日に行う。 \[1\]\n/u);
  assert.match(text, /\n\nLeft out 2 statements of the model that the passages do not bear out \(see --json\)\.\n?$/u);

  // What the question asks about, put onto a passage that does not name it: the search finds the
  // passage by 有給, but it says nothing of 有給休暇.
  const borrowed = await ask('有給休暇は何日？');
  assert.deepEqual(borrowed.knowledge_nest_rejected.at(-1), {
    text: '有給休暇は毎月最初の営業日に行う。',
    reason: 'names what the question asks about, which the passages it cites do not',
  });
  assert.equal(borrowed.output[0].content[0].text, '資料から回答の根拠を確認できませんでした。');

  // With nothing left, the answer declines and cites nothing.
  honest = false;
  const declined = await ask('ポンプの点検はいつ？');
  assert.equal(declined.output[0].content[0].text, '資料から回答の根拠を確認できませんでした。');
  assert.deepEqual(declined.output[0].content[0].annotations, []);
  assert.deepEqual(declined.knowledge_nest_citations, []);
  assert.equal(declined.knowledge_nest_rejected.length, 2);
});
