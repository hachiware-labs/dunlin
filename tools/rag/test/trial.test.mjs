// Spec 28.11 step 4c: Rust records and limits evaluation trials, and `use` puts the configuration
// file back when its transaction fails.
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { DatabaseSync } from 'node:sqlite';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { parse, stringify } from 'yaml';

const cli = fileURLToPath(new URL('../cli.mjs', import.meta.url));
const runNode = promisify(execFile);

test('limits evaluation trials and restores the configuration after a failed use', { timeout: 120_000 }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-trial-'));
  let state;
  // The database closes first, which Windows needs before removing its files.
  t.after(async () => {
    state?.close();
    await rm(root, { recursive: true, force: true, maxRetries: 5 });
  });
  const env = { ...process.env, DUNLIN_GLOBAL_CONFIG: join(root, 'global.yaml') };
  const run = (...args) => runNode(process.execPath, [cli, '--project', 'kb', ...args, '--json'], { cwd: root, env, encoding: 'utf8', timeout: 60_000 })
    .then(({ stdout }) => JSON.parse(stdout), (error) => ({ exitCode: error.code, ...JSON.parse(error.stdout) }));
  await runNode(process.execPath, [cli, 'init', 'rag', 'kb', '--no-embedding'], { cwd: root, env });
  await writeFile(join(root, 'kb', 'docs', 'guide.md'), '# 手順\n\nポンプの交換手順です。\n');
  assert.equal((await run('build')).status, 'succeeded');
  assert.equal((await run('build', '--tag', 'base')).status, 'succeeded');
  const added = await run('eval-data', 'add', '--question', '交換', '--doc', 'guide.md', '--quote', 'ポンプの交換手順です。');
  assert.equal(added.status, 'succeeded', JSON.stringify(added.errors));
  assert.equal((await run('eval-data', 'use', added.data.id)).status, 'succeeded');
  const configPath = join(root, 'kb', 'Dunlinfile.yaml');
  const config = parse(await readFile(configPath, 'utf8'));
  // The base build was a trial run.
  config.trial_limits = { max_runs: 2 };
  await writeFile(configPath, stringify(config));

  assert.equal((await run('eval', '--build', 'base')).status, 'succeeded');
  const limited = await run('eval', '--build', 'base');
  assert.equal(limited.exitCode, 4);
  assert.equal(limited.errors[0].code, 'TRIAL_LIMIT_REACHED');
  assert.deepEqual(limited.data, { limit: 'max_runs', maximum: 2, used: 2 });
  // Evaluating the active build is not a trial.
  assert.equal((await run('eval')).status, 'succeeded');
  const status = await run('status');
  assert.equal(status.data.trials.usage.runs, 2);
  state = new DatabaseSync(join(root, 'kb', '.dunlin', 'state.sqlite'));
  assert.deepEqual(state.prepare('SELECT operation, status FROM trial_runs ORDER BY started').all().map((row) => ({ ...row })),
    [{ operation: 'build', status: 'succeeded' }, { operation: 'eval', status: 'succeeded' }]);

  // A failure after the file was written puts the previous file back and records nothing.
  const before = await readFile(configPath, 'utf8');
  const adoptions = state.prepare('SELECT count(*) AS n FROM adoption_history').get().n;
  state.exec('ALTER TABLE serving_generations RENAME TO serving_generations_moved');
  const failed = await run('use', 'base');
  assert.equal(failed.status, 'failed');
  assert.equal(await readFile(configPath, 'utf8'), before);
  assert.equal(state.prepare('SELECT count(*) AS n FROM adoption_history').get().n, adoptions);
  state.exec('ALTER TABLE serving_generations_moved RENAME TO serving_generations');
  const used = await run('use', 'base');
  assert.equal(used.status, 'succeeded', JSON.stringify(used.errors));
  assert.equal(used.data.evaluation_status, 'evaluated');
  assert.equal(state.prepare('SELECT count(*) AS n FROM adoption_history').get().n, adoptions + 1);
  // The adopted file keeps `tokenizer`, which is a setting, not a secret.
  const adopted = parse(await readFile(configPath, 'utf8'));
  assert.equal(adopted.index.modules.find((module) => module.use === 'index.keyword').with.tokenizer, 'lindera');
  assert.deepEqual((await run('status')).warnings.filter((warning) => warning.code === 'ARG_DEFAULT'), []);

  // A configuration warning has no message; the terminal shows its place, code and hint.
  delete adopted.index.modules.find((module) => module.use === 'index.keyword').with.tokenizer;
  await writeFile(configPath, stringify(adopted));
  const { stderr } = await runNode(process.execPath, [cli, '--project', 'kb', 'status'], { cwd: root, env, encoding: 'utf8', timeout: 60_000 });
  assert.match(stderr, /^Warning: .*Dunlinfile\.yaml:\d+ index index\.keyword tokenizer — ARG_DEFAULT: Write the default value/m);
  assert.doesNotMatch(stderr, /undefined/);
});
