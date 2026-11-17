// Spec 28.10 steps 3a and 3b: the Rust search and answers give the results, context,
// configuration IDs, model requests, answers and errors of the JavaScript code they replaced
// (test/fixtures/search-reference.mjs and answer-reference.mjs).
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { DatabaseSync } from 'node:sqlite';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { parse, stringify } from 'yaml';

import { validateRag } from '../project-modules.mjs';
import { referenceAnswer, referenceAnswerModel } from './fixtures/answer-reference.mjs';
import { referenceOpen, referenceSearchJson } from './fixtures/search-reference.mjs';

const cli = fileURLToPath(new URL('../cli.mjs', import.meta.url));
const runNode = promisify(execFile);

const documents = {
  'guide.md': '# ポンプの点検\n\nポンプの電源を止めてから、カバーを外します。\n\n## 清掃\n\nフィルターを水で洗い、ポンプの吸い込み口を確認します。\n\n'
    + '## 交換\n\nパッキンが傷んでいれば交換します。部品番号は ABC-120 です。\n',
  'notes.txt': '保守の記録\n2024年4月にポンプの点検を行った。\n異音があったため、ファンを交換した。\n次回の点検は半年後とする。\n',
  'sub/long.md': '# 長い文書\n\n' + Array.from({ length: 24 }, (_, i) => `第${i + 1}節 ポンプ${i % 3 === 0 ? 'の点検' : ''}と記録用紙の確認。`).join('\n') + '\n',
  'other.md': '# 別の話題\n\n照明の交換は年に一度です。\n',
};

/** Embeddings from character counts, so that every run gives the same vectors. */
function embed(text) {
  const count = (word) => text.split(word).length - 1;
  return [count('ポンプ') + 1, count('点検') + 0.5, (text.length % 7) + 1];
}

/** Every request the model server received: `{ url, body }`. */
const requests = [];

/** The statements a test model returns for the evidence it was given. */
function facts({ question, evidence }) {
  if (question.includes('不正')) return { facts: [{ text: '範囲外', evidence_ids: [99] }] };
  return { facts: evidence.slice(0, 2).map(({ id, text }) => ({ text: ' ' + text.slice(0, 12) + ' ', evidence_ids: [id, id] })) };
}

/** A model server: Ollama embeddings and chat, the OpenAI Responses API and Strata chat completions. */
async function models(t) {
  const server = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const text = Buffer.concat(chunks).toString('utf8');
    requests.push({ url: request.url, body: text });
    response.setHeader('content-type', 'application/json');
    // The reranking model reverses the candidates.
    const answer = (input) => input.candidates ? JSON.stringify({ order: input.candidates.map(({ id }) => id).reverse() })
      : input.question.includes('壊れ') ? 'not json' : JSON.stringify(facts(input));
    if (request.url.endsWith('/api/embed')) {
      response.end(JSON.stringify({ embeddings: JSON.parse(text).input.map(embed) }));
    } else if (request.url.endsWith('/api/chat')) {
      const body = JSON.parse(text);
      const content = answer(JSON.parse(body.messages[1].content));
      response.end(JSON.stringify({ done: true, prompt_eval_count: 21, eval_count: 7, message: { content } }));
    } else if (request.url.endsWith('/responses')) {
      const input = JSON.parse(JSON.parse(text).input);
      response.end(JSON.stringify({ status: 'completed', usage: { input_tokens: 30, output_tokens: 8, input_tokens_details: { cached_tokens: 4 } },
        output: [{ type: 'reasoning' }, { type: 'message', content: [{ type: 'output_text', text: answer(input) }] }] }));
    } else if (request.url.endsWith('/chat/completions')) {
      const input = JSON.parse(JSON.parse(text).messages[1].content);
      response.end(JSON.stringify({ choices: [{ finish_reason: 'stop', message: { content: answer(input) } }],
        usage: { prompt_tokens: 9, completion_tokens: 3 } }));
    } else {
      response.statusCode = 404;
      response.end('{}');
    }
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  t.after(() => new Promise((resolve) => server.close(resolve)));
  return 'http://127.0.0.1:' + server.address().port;
}

/** The parts of an answer that differ on every call: its identifiers and time. */
function stable(response) {
  return JSON.stringify({ ...response, id: 'resp', created_at: 0, output: response.output.map((item) => ({ ...item, id: 'msg' })) });
}

const module = (use, args = {}) => ({ use, with: args });
const context = (args) => module('context.assemble', { budget_chars: 300, retrieval: 'fill_budget', max_candidates: 100, merge_overlaps: true,
  truncate_last: true, include_headings: true, ...args });

/** Creates and builds a project; returns helpers that run the CLI and the reference on it. */
async function project(t, root, { tokenizer, vector, hannoy, type = 'mcp' }) {
  const env = { ...process.env, DUNLIN_GLOBAL_CONFIG: join(root, 'global.yaml') };
  const run = (...args) => runNode(process.execPath, [cli, '--project', 'kb', ...args, '--json'], { cwd: root, env, encoding: 'utf8', timeout: 60_000 })
    .then(({ stdout }) => JSON.parse(stdout), (error) => ({ exitCode: error.code, ...JSON.parse(error.stdout) }));
  await runNode(process.execPath, [cli, 'init', type, 'kb'], { cwd: root, env, encoding: 'utf8', timeout: 60_000 });
  const base = join(root, 'kb');
  for (const [name, text] of Object.entries(documents)) {
    await mkdir(dirname(join(base, 'docs', name)), { recursive: true });
    await writeFile(join(base, 'docs', name), text);
  }
  const configPath = join(base, 'Dunlinfile.yaml');
  // dunlin add records the tool in the Dunlinfile, so it runs first.
  const tool = hannoy ? (await run('add', 'hannoy')).data : null;
  const config = parse(await readFile(configPath, 'utf8'));
  config.transform.modules[1].with = { ...config.transform.modules[1].with, size: 60, overlap: 20 };
  config.index.modules[0].with = tokenizer === 'lindera' ? { tokenizer, user_dictionary: null } : { tokenizer, short_query: 'bigram' };
  config.models = { embed: { provider: 'ollama', kind: 'embedding', model: 'e', base_url: vector },
    ranker: { provider: 'ollama', kind: 'chat', model: 'r', base_url: vector },
    ...(type === 'rag' && {
      answerer: { provider: 'ollama', kind: 'chat', model: 'a', base_url: vector },
      gpt: { provider: 'openai', kind: 'chat', model: 'g', base_url: vector, api_key_env: 'DUNLIN_PARITY_KEY' },
      strata: { provider: 'strata', kind: 'chat', model: 's', base_url: vector },
      limited: { provider: 'ollama', kind: 'chat', model: 'l', base_url: vector, max_input_bytes: 300 },
    }) };
  if (tool) {
    config.index.modules.push({ ...tool.configuration.index, with: { model: 'embed', ef_construction: 100 } });
  } else config.index.modules.push(module('index.vector', { model: 'embed' }));
  await writeFile(configPath, stringify(config));
  const built = await run('build');
  assert.equal(built.status, 'succeeded', JSON.stringify(built.errors));
  const state = new DatabaseSync(join(base, '.dunlin', 'state.sqlite'), { readOnly: true });
  const build = JSON.parse(state.prepare('SELECT manifest FROM project JOIN builds ON builds.id = project.build_id').get().manifest);
  state.close();
  const paths = { data: join(base, '.dunlin'), state: join(base, '.dunlin', 'state.sqlite'), artifacts: join(base, '.dunlin', 'artifacts'),
    corpus: join(base, '.dunlin', 'corpus') };
  const database = await referenceOpen(paths, build);
  t.after(() => database.close());
  let variants = 0;

  /**
   * Searches, or answers when `ask` names the answer.generate settings, with the CLI and with the
   * reference, using a candidate configuration. The model requests of both must be the same.
   */
  return async function compare(search, query, options = {}, ask = null) {
    const variant = { ...config, search: { modules: search }, ...(ask && { ask: { modules: [module('answer.generate', ask)] } }) };
    const path = join(base, `variant-${variants++}.yaml`);
    await writeFile(path, stringify(variant));
    const checked = validateRag(variant, path, null);
    assert.deepEqual(checked.errors, []);
    const args = [ask ? 'ask' : 'search', query, '--config', path];
    if (options.limit) args.push('--limit', String(options.limit));
    if (options.budgetChars) args.push('--budget-chars', String(options.budgetChars));
    for (const id of options.sourceIds ?? []) args.push('--source-id', id);
    const before = requests.length;
    const actual = await run(...args);
    const sent = requests.splice(before).filter(({ url }) => !url.endsWith('/api/embed') || !ask);
    const label = JSON.stringify({ search: search.map(({ use }) => use), query: query.slice(0, 20), options, ask });
    let expected;
    try {
      const project = { paths, config: variant, plan: checked.plan };
      expected = await referenceSearchJson(database, { ...build, search_models: undefined }, checked.plan.search, query, project, options);
      if (ask) expected = await referenceAnswer(project, expected, query, referenceAnswerModel(project));
    } catch (error) {
      assert.deepEqual(sent, requests.splice(before).filter(({ url }) => !url.endsWith('/api/embed') || !ask), label);
      const argument = error.code === 'INVALID_ARGUMENT';
      assert.equal(actual.status, 'failed', label);
      assert.equal(actual.exitCode, error.exitCode ?? (argument ? 2 : 1), label);
      assert.deepEqual(actual.errors[0], { code: error.exitCode ? error.code : argument ? 'INVALID_ARGUMENT' : 'EXECUTION_FAILED',
        message: error.message, ...(error.module && { module: error.module, section: error.section }) }, label);
      return actual;
    }
    assert.equal(actual.status, 'succeeded', label + ' ' + JSON.stringify(actual.errors));
    // The same requests to the models, byte for byte.
    assert.deepEqual(sent, requests.splice(before).filter(({ url }) => !url.endsWith('/api/embed') || !ask), label);
    // The same JSON text: values and key order.
    if (ask) assert.equal(stable(actual.data), stable(expected), label);
    else assert.equal(JSON.stringify(actual.data), JSON.stringify(expected), label);
    return actual.data;
  };
}

test('searches keyword and exact vector plans like the JavaScript search', { timeout: 300_000 }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-search-parity-'));
  const compare = await project(t, root, { tokenizer: 'lindera', vector: await models(t) });
  // After the open index closes: Windows keeps its files until then.
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const normalize = module('query.normalize', { unicode: 'nfkc' });
  const keyword = module('retrieval.keyword', { top_k: 10 });
  const vector = module('retrieval.vector', { model: 'embed', top_k: 10 });
  const plans = {
    keyword: [normalize, keyword, context()],
    topK: [normalize, module('retrieval.keyword', { top_k: 3 }), context({ budget_chars: 200, retrieval: 'top_k', merge_overlaps: false,
      truncate_last: false, include_headings: false })],
    hybrid: [normalize, keyword, vector, module('fusion.rrf', { k: 60 }), context({ budget_chars: 500 })],
    reranked: [normalize, keyword, vector, module('fusion.rrf', { k: 30 }), module('rerank.model', { model: 'ranker', candidates: 4 }),
      context({ budget_chars: 400 })],
    vector: [normalize, module('retrieval.vector', { model: 'embed', top_k: 3 }), context({ budget_chars: 150, max_candidates: 5 })],
  };
  let compared = 0;
  for (const [name, search] of Object.entries(plans)) {
    for (const query of ['ポンプ', 'ＡＢＣ', '点検 記録', '存在しない語']) {
      const data = await compare(search, query);
      if (query === 'ポンプ' && name !== 'vector') assert.ok(data.results.length > 1, name);
      compared++;
    }
  }
  for (const search of [plans.keyword, plans.hybrid]) {
    for (const options of [{ limit: 2 }, { budgetChars: 40 }, { sourceIds: ['docs'] }, { limit: 1, budgetChars: 1000 }]) {
      const data = await compare(search, 'ポンプ', options);
      assert.equal(data.warnings[0]?.code, 'SEARCH_OVERRIDES');
      compared++;
    }
  }
  // A query over the size limit fails in retrieval.keyword.
  assert.equal((await compare(plans.keyword, 'ポ'.repeat(1400))).errors[0].code, 'INVALID_ARGUMENT');
  t.diagnostic(`compared ${compared} searches`);
});

test('answers with Ollama, OpenAI and Strata like the JavaScript answer', { timeout: 300_000 }, async (t) => {
  process.env.DUNLIN_PARITY_KEY = 'parity-key';
  t.after(() => delete process.env.DUNLIN_PARITY_KEY);
  const root = await mkdtemp(join(tmpdir(), 'dunlin-answer-parity-'));
  const compare = await project(t, root, { tokenizer: 'lindera', vector: await models(t), type: 'rag' });
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const search = [module('query.normalize', { unicode: 'nfkc' }), module('retrieval.keyword', { top_k: 5 }), context({ budget_chars: 250 })];
  for (const model of ['answerer', 'gpt', 'strata']) {
    const response = await compare(search, 'ポンプの点検', {}, { model, cite: true });
    assert.ok(response.output[0].content[0].annotations.length > 0, model);
    assert.ok(response.usage.total_tokens > 0, model);
  }
  await compare(search, '交換は？', {}, { model: 'answerer', cite: false });
  // No context: the fixed answer without a model request.
  const declined = await compare([...search.slice(0, 2), context({ budget_chars: 5 })], 'ポンプ', {}, { model: 'answerer', cite: true });
  assert.equal(declined.usage, null);
  // Errors: a citation outside the context, output that is not JSON and a request over max_input_bytes.
  assert.equal((await compare(search, 'ポンプ 不正', {}, { model: 'strata', cite: true })).errors[0].code, 'INVALID_CITATION');
  assert.equal((await compare(search, 'ポンプ 壊れ', {}, { model: 'gpt', cite: true })).errors[0].code, 'INVALID_MODEL_OUTPUT');
  assert.equal((await compare(search, 'ポンプ', {}, { model: 'limited', cite: true })).errors[0].code, 'MODEL_INPUT_LIMIT');
  // rerank.model with the OpenAI and Strata request formats.
  for (const ranker of ['gpt', 'strata']) {
    await compare([...search.slice(0, 2), module('rerank.model', { model: ranker, candidates: 3 }), search[2]], 'ポンプ');
  }
});

test('searches the hannoy graph like the JavaScript search', { timeout: 300_000 }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-search-hannoy-'));
  const compare = await project(t, root, { tokenizer: 'trigram', vector: await models(t), hannoy: true });
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const plan = [module('query.normalize', { unicode: 'nfkc' }), module('retrieval.keyword', { top_k: 5 }),
    module('hannoy.search', { model: 'embed', top_k: 5, ef_search: 50 }), module('fusion.rrf', { k: 60 }), context()];
  for (const query of ['ポンプ', '交換']) assert.ok((await compare(plan, query)).results.length > 0);
  await compare(plan, 'ポンプ', { sourceIds: ['docs'], limit: 3 });
});
