// Spec 28.11 step 4e: eval-data generate in Rust sends the model the requests the reference in
// JavaScript sends (test/fixtures/eval-data-reference.mjs), byte for byte, and keeps and drops the
// same candidates. Two copies of one built project run generate, one through the CLI and one
// through the reference, against the same test model. The reference was the former JavaScript
// code; kinds, several quotes, the length of a quote and the order of excerpts were added to both.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFile } from 'node:child_process';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { cp, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { DatabaseSync } from 'node:sqlite';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { parse, stringify } from 'yaml';

import { referenceEnsureEvaluation, referenceManageEvaluationData } from './fixtures/eval-data-reference.mjs';
import { referenceOpen } from './fixtures/search-reference.mjs';

const cli = fileURLToPath(new URL('../cli.mjs', import.meta.url));
const runNode = promisify(execFile);
const sha256 = (text) => createHash('sha256').update(text).digest('hex');

const documents = {
  'guide.md': '# 手順\n\nポンプの交換手順です。\n\n## 点検\n\n月に一度ポンプを点検します。\n\n## 清掃\n\n月に一度ポンプを点検します。\n',
  // Longer than one excerpt, with lines, so that later passes continue where the first stopped.
  'long.md': Array.from({ length: 1500 }, (_, i) => `第${i + 1}節 記録用紙の確認と保管について。`).join('\n') + '\n',
  // One line whose excerpt boundary falls inside a surrogate pair.
  'emoji.txt': 'a'.repeat(3999) + '😀' + 'b'.repeat(50) + '\n末尾の行です。\n',
  'blank.txt': '   \n\n',
};

/**
 * A chat model that proposes, for each excerpt, a question quoting its first line, plus candidates
 * that must be dropped. Its second answer is not JSON.
 */
async function model(t) {
  const bodies = [];
  const server = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const body = Buffer.concat(chunks).toString('utf8');
    bodies.push(body);
    const excerpts = JSON.parse(JSON.parse(body).messages[1].content);
    const kinds = ['lexical', 'paraphrase', 'identifier'];
    const cases = excerpts.flatMap(({ doc, text }, i) => {
      const one = (question, ...quotes) => ({ question, kind: kinds[i % 3], evidence: quotes.map((quote) => ({ doc, quote })) });
      // Without the lone surrogate an excerpt can start with, which JSON.parse accepted.
      const whole = (value) => [...value].filter((unit) => !(unit.length === 1 && unit >= '\ud800' && unit <= '\udfff'));
      const lines = text.split('\n').map((value) => value.trim()).filter(Boolean);
      const quote = whole(lines[0] ?? '').slice(0, 30).join('');
      return [
        one(`${doc} ${i} ${quote}`, quote),
        one(`${doc} ${i} ${quote}`, quote),
        one(`${doc} ${i} missing`, 'not in the excerpt'),
        ...(whole(text).length > 300 ? [one(`${doc} ${i} long`, whole(text).slice(0, 301).join(''))] : []),
        ...(doc.endsWith('guide.md') ? [one(`${doc} twice`, '月に一度ポンプを点検します。'),
          one(`${doc} two`, lines[0], lines[1], lines[0]), one(`${doc} half`, lines[0], 'not in the excerpt')] : []),
      ];
    });
    const content = bodies.length === 2 ? 'not json' : JSON.stringify({ cases });
    response.setHeader('content-type', 'application/json');
    response.end(JSON.stringify({ done: true, message: { content } }));
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  t.after(() => new Promise((resolve) => { server.closeAllConnections(); server.close(resolve); }));
  return { url: 'http://127.0.0.1:' + server.address().port, bodies };
}

function normalizer() {
  const ids = new Map();
  const versions = new Map();
  const placeholder = (map, prefix) => (value) => {
    if (!map.has(value)) map.set(value, prefix + map.size);
    return map.get(value);
  };
  return (text) => text.replace(/"(created_at|at)":"[^"]*"/gu, '"$1":"time"')
    .replace(/ed-[0-9a-f]{64}/gu, placeholder(ids, 'dataset-'))
    .replace(/(?<="version":")[0-9a-f]{64}/gu, placeholder(versions, 'version-'));
}

test('generates evaluation candidates like the reference in JavaScript', { timeout: 300_000 }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'knowledge-nest-eval-generate-parity-'));
  const { url, bodies } = await model(t);
  const env = { ...process.env, KNOWLEDGE_NEST_GLOBAL_CONFIG: join(root, 'global.yaml') };
  await runNode(process.execPath, [cli, 'init', 'rag', 'kb', '--no-embedding'], { cwd: root, env });
  const base = join(root, 'kb');
  for (const [name, content] of Object.entries(documents)) await writeFile(join(base, 'docs', name), content);
  const configPath = join(base, 'KnowledgeNestfile.yaml');
  const config = parse(await readFile(configPath, 'utf8'));
  config.models = { ...config.models, local: { provider: 'ollama', kind: 'chat', model: 'test', base_url: url },
    broken: { provider: 'ollama', kind: 'chat', model: 'test', base_url: 'not a url' } };
  await writeFile(configPath, stringify(config));
  await runNode(process.execPath, [cli, '--project', 'kb', 'build'], { cwd: root, env });
  await cp(base, join(root, 'ref'), { recursive: true, preserveTimestamps: true });
  await runNode(process.execPath, [cli, '--project', 'ref', 'build'], { cwd: root, env });
  const run = (project, ...args) => runNode(process.execPath, [cli, '--project', project, 'eval-data', ...args, '--json'],
    { cwd: root, env, encoding: 'utf8', timeout: 60_000 })
    .then(({ stdout }) => JSON.parse(stdout), (error) => ({ exitCode: error.code, ...JSON.parse(error.stdout) }));

  const refBase = join(root, 'ref');
  const paths = { data: join(refBase, '.knowledge-nest'), state: join(refBase, '.knowledge-nest', 'state.sqlite'),
    artifacts: join(refBase, '.knowledge-nest', 'artifacts'), corpus: join(refBase, '.knowledge-nest', 'corpus') };
  const db = new DatabaseSync(paths.state);
  db.exec('PRAGMA busy_timeout = 500');
  const actualDb = new DatabaseSync(join(base, '.knowledge-nest', 'state.sqlite'), { readOnly: true });
  // The databases close first, which Windows needs before removing their files.
  t.after(async () => {
    db.close();
    actualDb.close();
    await rm(root, { recursive: true, force: true, maxRetries: 5 });
  });
  const reference = async (action, positionals, values = {}) => {
    const text = await readFile(join(refBase, 'KnowledgeNestfile.yaml'), 'utf8');
    const project = { root: refBase, config: parse(text), configPath: join(refBase, 'KnowledgeNestfile.yaml'), configDigest: sha256(text), paths };
    referenceEnsureEvaluation(db);
    return referenceManageEvaluationData(db, project, action, positionals, values, () => referenceOpen(paths,
      JSON.parse(db.prepare('SELECT manifest FROM project JOIN builds ON builds.id = project.build_id').get().manifest)));
  };
  const [actualIds, expectedIds] = [normalizer(), normalizer()];

  /** Generates on both copies and compares the model requests, outputs or errors. */
  const same = async (values) => {
    const label = JSON.stringify(values);
    const args = Object.entries(values).flatMap(([key, value]) => ['--' + key, value]);
    const first = bodies.length;
    const actual = await run('kb', 'generate', ...args);
    const sent = bodies.splice(first);
    let expected;
    let failure;
    try {
      expected = await reference('generate', [], values);
    } catch (error) {
      failure = error;
    }
    assert.deepEqual(sent, bodies.splice(first), label + ' requests');
    if (failure) {
      assert.equal(actual.status, 'failed', label + ': ' + failure.message);
      assert.equal(actual.errors[0].code, failure.code, label);
      assert.equal(actual.errors[0].message, failure.message, label);
      assert.equal(actual.exitCode, failure.exitCode, label);
      return null;
    }
    assert.equal(actual.status, 'succeeded', label + ' ' + JSON.stringify(actual.errors));
    assert.equal(actualIds(JSON.stringify(actual.data)), expectedIds(JSON.stringify(expected)), label);
    return { actual: actual.data, sent };
  };

  await same({});
  await same({ model: 'missing' });
  await same({ model: 'local', count: '0' });
  await same({ model: 'local', doc: 'docs:none.md' });
  await same({ model: 'broken' });
  await same({ model: 'local', kind: 'other' });
  const only = await same({ model: 'local', count: '3', kind: 'identifier' });
  assert.ok(only.actual.dropped.kind_not_requested > 0);
  assert.deepEqual(only.actual.kinds, { lexical: 0, paraphrase: 0, identifier: only.actual.generated });
  assert.ok(only.sent.every((body) => body.includes('Write only identifier questions. ')));
  const generated = await same({ model: 'local', count: '30' });
  assert.ok(generated.sent.length >= 3, 'several requests');
  assert.ok(generated.sent.some((body) => body.includes('\\ud83d')), 'a lone surrogate at an excerpt boundary');
  assert.deepEqual(Object.keys(generated.actual.dropped).sort(),
    ['duplicate', 'invalid_model_output', 'quote_not_in_excerpt', 'quote_not_unique', 'quote_too_long']);
  assert.ok(Object.values(generated.actual.kinds).every((count) => count > 0), 'every kind');
  // A second run reads documents that have no cases yet first, then the parts of long.md that
  // have the fewest quotes of cases.
  await same({ model: 'local', count: '5', doc: 'long.md' });
  await same({ model: 'local', count: '3' });

  const parsed = (database) => JSON.stringify(database.prepare('SELECT id, data FROM evaluation_datasets ORDER BY rowid').all()
    .map(({ id, data }) => ({ id, data: JSON.parse(data) })));
  assert.equal(actualIds(parsed(actualDb)), expectedIds(parsed(db)));
  for (const { id, data } of actualDb.prepare('SELECT id, data FROM evaluation_datasets').all()) {
    assert.equal(id, 'ed-' + sha256(JSON.stringify(JSON.parse(data).cases)));
  }
});
