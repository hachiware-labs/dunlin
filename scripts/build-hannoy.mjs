import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { chmod, mkdir, readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../', import.meta.url));
const target = join(root, 'target', 'hannoy');
execFileSync('cargo', ['build', '--release', '--locked', '--manifest-path', join(root, 'tools', 'hannoy', 'Cargo.toml'),
  '--target-dir', target], { cwd: root, stdio: 'inherit' });
const suffix = process.platform === 'win32' ? '.exe' : '';
const file = 'knowledge-nest-hannoy-' + process.platform + '-' + process.arch + suffix;
const output = join(root, 'dist', 'tools');
const bytes = await readFile(join(target, 'release', 'knowledge-nest-hannoy' + suffix));
await mkdir(output, { recursive: true });
await writeFile(join(output, file), bytes);
await chmod(join(output, file), 0o755);
const version = /^version = "(.+)"$/mu.exec(await readFile(join(root, 'tools', 'hannoy', 'Cargo.toml'), 'utf8'))[1];
await writeFile(join(output, file + '.json'), JSON.stringify({ version, protocol: 1, file,
  sha256: createHash('sha256').update(bytes).digest('hex') }) + '\n');
