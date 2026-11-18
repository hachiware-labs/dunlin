import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { copyFile, cp, mkdir, mkdtemp, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const project = fileURLToPath(new URL('../', import.meta.url));
const output = join(project, 'dist', 'packages');
const stage = await mkdtemp(join(tmpdir(), 'dunlin-package-'));
const cache = join(stage, 'npm-cache');
const npmCli = process.env.npm_execpath;
assert.ok(npmCli, 'Run this script via npm run test:package');

function npm(args, cwd = project) {
  return execFileSync(process.execPath, [npmCli, ...args], { cwd, encoding: 'utf8' });
}

async function pack(directory, destination) {
  await mkdir(destination, { recursive: true });
  const [result] = JSON.parse(npm([
    'pack', directory, '--pack-destination', destination, '--json', '--offline', '--ignore-scripts',
  ]));
  return { ...result, path: join(destination, result.filename) };
}

async function installAndSearch(name, tarballs) {
  const preparation = join(stage, name + '-preparation');
  const consumer = join(stage, name);
  for (const directory of [preparation, consumer]) {
    await mkdir(directory);
    await writeFile(join(directory, 'package.json'), '{"private":true,"type":"module"}');
  }
  const options = ['--cache', cache, '--ignore-scripts', '--no-audit', '--no-fund'];
  // Populate the cache, then install the same tarballs into a fresh offline consumer.
  npm(['install', ...options, ...tarballs], preparation);
  npm(['install', '--offline', ...options, ...tarballs], consumer);
  const installed = await readdir(join(consumer, 'node_modules'));
  assert.ok(installed.includes('zod'));

  await writeFile(join(consumer, 'source.md'), '# 認証\nOAuthの根拠\n[Policy](dunlin://policies/policy.txt)');
  await writeFile(join(consumer, 'policy.txt'), 'Linked policy evidence');
  await copyFile(join(project, 'examples', 'search.mjs'), join(consumer, 'search.mjs'));
  const result = execFileSync(process.execPath, ['search.mjs', './source.md', 'OAuth'], {
    cwd: consumer,
    encoding: 'utf8',
  });
  assert.match(result, /OAuthの根拠/u);
  const cli = join(consumer, 'node_modules', 'dunlin', 'tools', 'rag', 'cli.mjs');
  const env = { ...process.env, DUNLIN_GLOBAL_CONFIG: join(consumer, 'global.yaml') };
  const run = (...args) => execFileSync(process.execPath, [cli, ...args], { cwd: consumer, env, encoding: 'utf8' });
  await mkdir(join(consumer, 'manuals'));
  await writeFile(join(consumer, 'manuals', 'guide.md'), '# 認証\n作業手順を確認する。\n');
  run('init', 'mcp', 'kb', '--source', 'manuals');
  run('config', 'set', 'llm', '--model', 'packaged-test-model');
  run('init', 'rag', 'rag', '--source', 'manuals');
  assert.match(await readFile(join(consumer, 'rag', 'Dunlinfile.yaml'), 'utf8'), /model: packaged-test-model/u);
  run('init', 'wiki', 'wiki', '--purpose', 'Wiki');
  run('init', 'para', 'para', '--purpose', 'PARA');
  run('--project', 'kb', 'build');
  assert.match(run('--project', 'kb', 'search', '手順'), /作業手順/u);
  assert.match(run('--project', 'kb', 'search', '作業手順はどのように確認しますか'), /作業手順/u);
  const config = JSON.parse(run('--project', 'kb', 'serve', '--print-config', '--json'));
  assert.equal(config.schema_version, 1);
  assert.ok(config.data.args.includes('serve'));
  run('--project', 'rag', 'build');
  const rag = JSON.parse(run('--project', 'rag', 'search', '手順', '--json'));
  assert.equal(rag.data.results[0].document_id, 'docs:guide.md');
  assert.equal(JSON.parse(run('--project', 'rag', 'serve', '--print-config', '--json')).data.url, 'http://127.0.0.1:4318/mcp');
  assert.equal(JSON.parse(run('--project', 'kb', 'add', 'hannoy', '--json')).data.version, '0.2.0');
  execFileSync(process.execPath, ['--input-type=module', '-e', `
    import assert from 'node:assert/strict';
    import { Dunlin } from 'dunlin';
    const database = await Dunlin.open('./knowledge');
    try {
      const report = await database.optimize({
        evaluationSet: [{ question: 'OAuth 認証', doc: 'docs:source.md', quote: 'OAuthの根拠' }],
        p95LatencyBudgetMs: 500,
      });
      assert.equal(report.candidates.length, 2);
      assert.ok(report.selectedPlanId);
      await database.activatePlan(report.selectedPlanId);
      assert.equal((await database.search('OAuth')).planId, report.selectedPlanId);
      await database.addSource({ id: 'policies', kind: 'filesystem', path: './policy.txt' });
      const explored = await database.explore('OAuth', { maxSteps: 5 });
      assert.equal(explored.status, 'complete');
      assert.deepEqual(explored.evidence.map(({ evidence }) => evidence.source.sourceId), ['docs', 'policies']);
      const wiki = await database.wiki.build({
        id: 'auth/oauth', title: 'OAuth',
        documents: explored.evidence.map(({ evidence }) => evidence.source),
      });
      assert.equal(wiki.citations.length, 2);
      const removed = await database.syncDocuments('policies', []);
      assert.deepEqual(removed.deleted, ['policy.txt']);
      assert.deepEqual(removed.wiki.regenerated, [{ id: wiki.id, revision: 2, status: 'insufficient_evidence' }]);
      assert.equal((await database.wiki.get(wiki.id)).status, 'insufficient_evidence');
      assert.deepEqual(await database.wiki.affected(), []);
    } finally {
      await database.close();
    }
  `], { cwd: consumer, encoding: 'utf8' });
}

try {
  const bundled = await pack(project, join(output, 'bundled'));
  for (const name of ['lindera-LICENSE.txt', 'ipadic-NOTICE.txt']) {
    assert.ok(bundled.files.some(({ path }) => path === 'licenses/' + name));
  }
  const addons = bundled.files.filter(({ path }) => path.endsWith('.node'));
  assert.equal(addons.length, 1, 'Build one native target in a clean dist directory before packing');
  await installAndSearch('bundled-consumer', [bundled.path]);

  const binary = addons[0].path;
  const configs = await Promise.all((await readdir(join(project, 'npm'), { withFileTypes: true }))
    .filter((entry) => entry.isDirectory())
    .map(async ({ name }) => JSON.parse(await readFile(join(project, 'npm', name, 'package.json'), 'utf8'))));
  const platform = configs.find(({ main }) => main === basename(binary));
  assert.ok(platform, `No generated platform package for ${binary}`);

  const nativeDir = join(stage, 'native');
  await mkdir(nativeDir);
  await copyFile(join(project, binary), join(nativeDir, platform.main));
  await cp(join(project, 'dist', 'tools'), join(nativeDir, 'tools'), { recursive: true });
  await cp(join(project, 'licenses'), join(nativeDir, 'licenses'), { recursive: true });
  await writeFile(join(nativeDir, 'package.json'), JSON.stringify({ ...platform, files: [...platform.files, 'tools/*', 'licenses/*.txt'], private: true }));
  const nativeTar = await pack(nativeDir, join(output, 'split'));

  const rootDir = join(stage, 'root');
  const rootManifest = JSON.parse(await readFile(join(project, 'package.json'), 'utf8'));
  rootManifest.files = rootManifest.files.filter((path) => !['dist/*.node', 'dist/tools/*'].includes(path));
  rootManifest.optionalDependencies = Object.fromEntries(
    configs.map(({ name }) => [name, rootManifest.version]),
  );
  await mkdir(rootDir);
  await writeFile(join(rootDir, 'package.json'), JSON.stringify(rootManifest));
  await cp(join(project, 'examples'), join(rootDir, 'examples'), { recursive: true });
  for (const { path } of bundled.files) {
    const rootDoc = path === 'LICENSE' || /^[^/]+\.md$/u.test(path);
    if ((path.startsWith('dist/') && !path.startsWith('dist/tools/') && !path.endsWith('.node')) || /^(tools\/rag|docs|licenses)\//u.test(path) || rootDoc) {
      const target = join(rootDir, path);
      await mkdir(dirname(target), { recursive: true });
      await copyFile(join(project, path), target);
    }
  }
  const rootTar = await pack(rootDir, join(output, 'split'));
  assert.ok(rootTar.files.every(({ path }) => !path.endsWith('.node')));
  await installAndSearch('split-consumer', [rootTar.path, nativeTar.path]);
  console.log(`Verified offline npm installation of bundled and split packages in ${output}`);
} finally {
  await rm(stage, { recursive: true, force: true, maxRetries: 5 });
}

