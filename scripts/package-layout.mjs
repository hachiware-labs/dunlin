import { copyFile, cp, mkdir, readdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';

// The published layout: one root package without native code, and one package
// per platform that carries the addon, its helper tools and their licenses.

export async function platformPackages(project) {
  const entries = await readdir(join(project, 'npm'), { withFileTypes: true });
  return Promise.all(entries.filter((entry) => entry.isDirectory())
    .map(async ({ name }) => JSON.parse(await readFile(join(project, 'npm', name, 'package.json'), 'utf8'))));
}

export async function stageNative({ project, directory, platform, binary, tools, release = false }) {
  await mkdir(directory, { recursive: true });
  await copyFile(binary, join(directory, platform.main));
  await cp(tools, join(directory, 'tools'), { recursive: true });
  await cp(join(project, 'licenses'), join(directory, 'licenses'), { recursive: true });
  const root = JSON.parse(await readFile(join(project, 'package.json'), 'utf8'));
  await writeFile(join(directory, 'package.json'), JSON.stringify({
    ...platform, version: root.version, files: [...platform.files, 'tools/*', 'licenses/*.txt'],
    ...(release ? { repository: root.repository } : { private: true }),
  }, null, 2) + '\n');
}

// `files` is the file list of the bundled package as reported by `npm pack --json`.
export async function stageRoot({ project, directory, files, platforms, release = false }) {
  const manifest = JSON.parse(await readFile(join(project, 'package.json'), 'utf8'));
  manifest.files = manifest.files.filter((path) => !['dist/*.node', 'dist/tools/*'].includes(path));
  manifest.optionalDependencies = { ...(release && manifest.optionalDependencies),
    ...Object.fromEntries(platforms.map(({ name }) => [name, manifest.version])) };
  if (release) for (const key of ['private', 'scripts', 'devDependencies']) delete manifest[key];
  await mkdir(directory, { recursive: true });
  await writeFile(join(directory, 'package.json'), JSON.stringify(manifest, null, 2) + '\n');
  await cp(join(project, 'examples'), join(directory, 'examples'), { recursive: true });
  for (const { path } of files) {
    const rootDoc = path === 'LICENSE' || /^[^/]+\.md$/u.test(path);
    if ((path.startsWith('dist/') && !path.startsWith('dist/tools/') && !path.endsWith('.node')) || /^(tools\/rag|docs|licenses)\//u.test(path) || rootDoc) {
      const target = join(directory, path);
      await mkdir(dirname(target), { recursive: true });
      await copyFile(join(project, path), target);
    }
  }
  return manifest;
}
