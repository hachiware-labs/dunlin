// Assembles the npm tarballs for a release from the native builds of every platform.
//
//   npm run release:prepare -- --artifacts <directory> [--partial]
//
// <directory> holds one folder per platform, each with the addon (knowledge-nest.<platform>.node)
// and its tools/ directory, as uploaded by the release workflow. --partial accepts a
// subset of the platforms, for trying the layout on one machine (for example --artifacts dist).
// Nothing is published: the tarballs are written to dist/release/.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { access, mkdir, mkdtemp, readdir, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

import { platformPackages, stageNative, stageRoot } from './package-layout.mjs';

const project = fileURLToPath(new URL('../', import.meta.url));
const { values } = parseArgs({ options: { artifacts: { type: 'string' }, partial: { type: 'boolean' } } });
assert.ok(values.artifacts, 'Use --artifacts <directory>');
const artifacts = resolve(values.artifacts);
const npmCli = process.env.npm_execpath;
assert.ok(npmCli, 'Run this script via npm run release:prepare');
const npm = (...args) => execFileSync(process.execPath, [npmCli, ...args], { cwd: project, encoding: 'utf8' });
const exists = (path) => access(path).then(() => true, () => false);

const manifest = JSON.parse(await readFile(join(project, 'package.json'), 'utf8'));
const server = JSON.parse(await readFile(join(project, 'server.json'), 'utf8'));
assert.equal(server.name, manifest.mcpName, 'server.json name must equal mcpName in package.json');
assert.equal(server.version, manifest.version, 'server.json version must equal the package version');
assert.ok([...server.description].length <= 100, 'The MCP Registry limits the description to 100 characters');
for (const entry of server.packages) {
  assert.equal(entry.identifier, manifest.name, 'server.json must point at this npm package');
  assert.equal(entry.version, manifest.version, 'server.json package version must equal the package version');
}

const folders = [artifacts, ...(await readdir(artifacts, { withFileTypes: true }))
  .filter((entry) => entry.isDirectory()).map(({ name }) => join(artifacts, name))];
const platforms = await platformPackages(project);
const output = join(project, 'dist', 'release');
const stage = await mkdtemp(join(tmpdir(), 'knowledge-nest-release-'));
try {
  await rm(output, { recursive: true, force: true });
  await mkdir(join(output, 'platforms'), { recursive: true });
  await mkdir(join(output, 'root'));
  const pack = (directory, destination) => JSON.parse(npm('pack', directory, '--pack-destination', destination, '--json', '--ignore-scripts'))[0];
  const built = [];
  for (const platform of platforms) {
    let folder;
    for (const candidate of folders) if (await exists(join(candidate, platform.main))) folder = candidate;
    if (!folder) {
      assert.ok(values.partial, 'No build of ' + platform.main + ' in ' + artifacts);
      continue;
    }
    const directory = join(stage, platform.name);
    await stageNative({ project, directory, platform, binary: join(folder, platform.main), tools: join(folder, 'tools'), release: true });
    const packed = pack(directory, join(output, 'platforms'));
    assert.ok(packed.files.some(({ path }) => path === platform.main));
    assert.ok(packed.files.some(({ path }) => path.startsWith('tools/knowledge-nest-hannoy-')), 'No index tool in ' + platform.name);
    built.push(packed.filename);
  }
  assert.ok(built.length, 'No platform build found in ' + artifacts);

  const [{ files }] = JSON.parse(npm('pack', project, '--dry-run', '--json', '--ignore-scripts'));
  const root = join(stage, 'root');
  const published = await stageRoot({ project, directory: root, files, platforms, release: true });
  assert.equal(published.private, undefined);
  const packed = pack(root, join(output, 'root'));
  assert.ok(packed.files.every(({ path }) => !path.endsWith('.node')));
  assert.ok(packed.files.some(({ path }) => path === 'dist/native.cjs'));
  console.log('Platform packages (publish these first):\n' + built.map((name) => '  dist/release/platforms/' + name).join('\n'));
  console.log('Root package (publish last):\n  dist/release/root/' + packed.filename);
  if (values.partial) console.log('Partial release: ' + (platforms.length - built.length) + ' platform package(s) are missing. Do not publish.');
} finally {
  await rm(stage, { recursive: true, force: true, maxRetries: 5 });
}
