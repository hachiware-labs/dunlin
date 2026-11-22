import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

import { parse, stringify } from 'yaml';

const cli = fileURLToPath(new URL('../cli.mjs', import.meta.url));
const runNode = promisify(execFile);

test('diagnoses evaluation misses and measures whole and partial queries against a separate dictionary build', { timeout: 60_000 }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'knowledge-nest-dictionary-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const env = { ...process.env, KNOWLEDGE_NEST_GLOBAL_CONFIG: join(root, 'global.yaml') };
  const run = (...args) => runNode(process.execPath, [cli, ...args], { cwd: root, env, encoding: 'utf8', timeout: 10_000 });
  const json = async (...args) => JSON.parse((await run('--project', 'kb', ...args, '--json')).stdout).data;
  const rejects = (promise, code, hint) => assert.rejects(promise, (error) => {
    const [failure] = JSON.parse(error.stdout).errors;
    return failure.code === code && (!hint || failure.message.includes(hint));
  });
  await run('init', 'mcp', 'kb');
  await rejects(json('dictionary', 'suggest'), 'EVAL_DATA_REQUIRED', 'eval-data add');
  const docs = join(root, 'kb', 'docs');
  const configPath = join(root, 'kb', 'KnowledgeNestfile.yaml');
  const target = 'docs:product.txt';
  const quote = '製品東京大学は限定品';
  await Promise.all([
    writeFile(join(docs, 'product.txt'), quote + '\n'),
    writeFile(join(docs, 'lecture.txt'), '東京大学で講演します。\n'),
    writeFile(join(docs, 'device.txt'), '製品名はみなとです。\n'),
    writeFile(join(docs, 'maintenance.txt'), '設備の点検は毎月実施します。\n'),
    writeFile(join(docs, 'index.txt'), '点検 点検 点検 点検 点検\n'),
  ]);
  const buildConfig = parse(await readFile(configPath, 'utf8'));
  buildConfig.search.modules.find(({ use }) => use === 'retrieval.keyword').with.top_k = 1;
  buildConfig.search.modules.find(({ use }) => use === 'context.assemble').with.retrieval = 'top_k';
  await writeFile(configPath, stringify(buildConfig));
  const baseline = await json('build');
  assert.equal((await json('tag', 'baseline-build')).build_id, baseline.build_id);
  for (const [question, doc, expected] of [
    ['東京大学', target, quote],
    ['みなとの仕様は', 'docs:device.txt', '製品名はみなとです。'],
    ['大学', 'docs:lecture.txt', '東京大学で講演します。'],
    ['点検', 'docs:maintenance.txt', '点検は毎月実施します。'],
  ]) await json('eval-data', 'add', '--question', question, '--doc', doc, '--quote', expected);
  await rejects(json('dictionary', 'suggest'), 'REPORT_REQUIRED', 'nest eval');

  const config = parse(await readFile(configPath, 'utf8'));
  config.eval.modules.find(({ use }) => use === 'metric.evidence_recall').with.at = ['context', 1];
  const active = stringify(config);
  await writeFile(configPath, active);
  const report = await json('eval', '--tag', 'base-eval');
  assert.equal(report.scores['evidence_recall@context'], 0);
  const suggestion = await json('dictionary', 'suggest', '--report', 'base-eval');
  assert.equal(suggestion.build_id, baseline.build_id);
  assert.deepEqual(suggestion, await json('dictionary', 'suggest'));
  assert.deepEqual(suggestion.counts, { dictionary: 2, partial_match: 1, other: 1, unresolved: 0 });
  assert.deepEqual(suggestion.candidates.map(({ surface }) => surface).sort(), ['みなと', '東京大学']);
  assert.equal(suggestion.findings[0].reason, 'below_evaluation_cutoff');
  assert.deepEqual(suggestion.findings[0].matched_terms, ['点検']);
  const fragment = suggestion.candidates.find(({ surface }) => surface === 'みなと').examples[0];
  assert.deepEqual(fragment.question_tokens.map(({ surface }) => surface), ['みな', 'と']);
  assert.equal(fragment.question_tokens[1].term, null);
  assert.deepEqual(fragment.queries, { whole: 'みなとの仕様は', partial: 'みなの仕様は' });
  const candidate = suggestion.candidates.find(({ surface }) => surface === '東京大学');
  const [example] = candidate.examples;
  assert.equal(example.doc, target);
  assert.equal(example.quote, quote);
  assert.deepEqual(example.question_tokens.map(({ surface }) => surface), ['東京大学']);
  assert.deepEqual(example.evidence_tokens.map(({ surface }) => surface), ['東京', '大学']);
  assert.deepEqual(example.queries, { whole: '東京大学', partial: '大学' });
  const text = (await run('--project', 'kb', 'dictionary', 'suggest')).stdout;
  assert.match(text, /部分一致 1件/u);
  assert.match(text, /語全体の質問 東京大学\s+部分語の質問 大学/u);

  await mkdir(join(root, 'kb', 'candidates'));
  const candidateFile = 'kb/candidates/dict.yaml';
  const candidatePath = join(root, candidateFile);
  const selected = join(root, 'selected.csv');
  config.source.modules.find(({ use }) => use === 'source.filesystem').with.path = '../docs';
  const candidateInput = '# Trial configuration comment\n' + stringify(config);
  await writeFile(candidatePath, candidateInput);
  await writeFile(selected, '選択,名詞\n');
  await rejects(json('dictionary', 'add', 'selected.csv', '--config', candidateFile), 'INVALID_USER_DICTIONARY');
  assert.equal(await readFile(candidatePath, 'utf8'), candidateInput);
  await writeFile(selected, candidate.csv + '\n');
  await rejects(json('dictionary', 'add', 'selected.csv', '--config', 'kb/KnowledgeNestfile.yaml'), 'CANDIDATE_REQUIRED');
  const added = await json('dictionary', 'add', 'selected.csv', '--config', candidateFile);
  assert.match(await readFile(candidatePath, 'utf8'), /^# Trial configuration comment\n/u);
  assert.equal(await readFile(added.user_dictionary, 'utf8'), candidate.csv + '\n');
  assert.equal(await readFile(configPath, 'utf8'), active);
  assert.deepEqual(added, await json('dictionary', 'add', 'selected.csv', '--config', candidateFile));

  const trial = await json('build', '--from', 'index', '--input', 'baseline-build', '--config', candidateFile, '--tag', 'dict');
  assert.equal(trial.source_id, baseline.source_id);
  assert.equal(trial.transform_id, baseline.transform_id);
  assert.equal(trial.serving_changed, false);
  assert.equal((await json('status')).build_id, baseline.build_id);
  const rank = async (question, build) => (await json('search', question, '--build', build)).results
    .find(({ document_id }) => document_id === target)?.rank ?? null;
  const ranks = {};
  for (const [kind, question] of Object.entries(example.queries)) {
    ranks[kind] = { baseline: await rank(question, baseline.build_id), trial: await rank(question, 'dict') };
  }
  assert.deepEqual(ranks, { whole: { baseline: null, trial: 1 }, partial: { baseline: 1, trial: null } });
  t.diagnostic(`Target ranks within top 1: ${JSON.stringify(ranks)}`);
  const trialReport = await json('eval', '--build', 'dict', '--tag', 'dict-eval');
  assert.equal(trialReport.scores['evidence_recall@context'], 1 / 4);
  const comparison = await json('report', 'compare', 'base-eval', 'dict-eval');
  assert.equal(comparison.comparable, true);
  assert.deepEqual(comparison.cases.map(({ status }) => status).sort(), ['recovered', 'still_missing', 'still_missing', 'still_missing']);
  assert.equal(comparison.cases.find(({ status }) => status === 'recovered').question, '東京大学');
  assert.deepEqual((await json('dictionary', 'suggest', '--report', report.id)).candidates, suggestion.candidates);
  const trialSuggestions = await json('dictionary', 'suggest', '--report', 'dict-eval');
  assert.equal(trialSuggestions.build_id, trial.build_id);
  assert.deepEqual(trialSuggestions.counts, { dictionary: 1, partial_match: 1, other: 1, unresolved: 0 });

  const extra = '密閉式架台,名詞,ミッペイシキカダイ\n';
  await writeFile(selected, extra);
  const merged = await json('dictionary', 'add', 'selected.csv', '--config', candidateFile);
  assert.equal(await readFile(merged.user_dictionary, 'utf8'), candidate.csv + '\n' + extra);
  assert.equal(await readFile(added.user_dictionary, 'utf8'), candidate.csv + '\n');
});
