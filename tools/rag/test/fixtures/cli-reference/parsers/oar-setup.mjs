// Prepares oar-ocr: ONNX Runtime and the PP-OCRv6 small models, outside the package (spec 4.1, 25.4).
// Every file is pinned by SHA-256. The Rust engine reads oar-ocr.json in the prepared folder.
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, readdir, rename, rm, stat, writeFile } from 'node:fs/promises';
import { dirname, join, relative } from 'node:path';

import { downloadVerified } from './download.mjs';
import { dataRoot } from './lighton-setup.mjs';

/** The tool version: the oar-ocr crate and the model set. Part of the OCR cache key. */
export const oarOcrVersion = '0.10.2-ppocrv6-small';

const runtimeVersion = '1.28.3';
const runtimeRelease = `https://github.com/microsoft/onnxruntime/releases/download/v${runtimeVersion}`;
const runtimes = {
  'win32/x64': ['onnxruntime-win-x64', 'zip', '1d6fab48e85f948436af7c8c971d2c145cf224e2c444755dec894f8b0de11a83'],
  'win32/arm64': ['onnxruntime-win-arm64', 'zip', '76d5e5a23fb7fdbc2ca0c3214bad1b58b2a41016c7052e74e60ed8004bea71b1'],
  'linux/x64': ['onnxruntime-linux-x64', 'tgz', 'db14e4863bd37893fc59729d986ab2a0d043d10b7d44da1913c4982b7e3d009c'],
  'linux/arm64': ['onnxruntime-linux-aarch64', 'tgz', '6c6b1ae96d7b0be9f555092857c6a4f2b0b5587a5298d732d9e52e8253038ce7'],
  'darwin/arm64': ['onnxruntime-osx-arm64', 'tgz', 'c436bd9f47dbce6f6311ccc21de97f829b3c862c09d24aa321958cb72f0a1d32'],
};
const modelRelease = 'https://github.com/GreatV/oar-ocr/releases/download/v0.7.0';
const models = {
  detection: ['pp-ocrv6_small_det.onnx', 'd73e0058b7a8086bbd57f3d10b8bcd4ff95363f67e06e2762b5e814fe9c9410e'],
  recognition: ['pp-ocrv6_small_rec.onnx', '5435fd747c9e0efe15a96d0b378d5bd157e9492ed8fd80edf08f30d02fa24634'],
  dictionary: ['ppocrv6_dict.txt', 'b5f2bfe2bdd9448429e3e82b51c789775d9b42f2403d082b00662eb77e401c5d'],
};

/** The folder oar-ocr uses, beside the lightonocr folder (KNOWLEDGE_NEST_OCR_HOME moves both). */
export function oarOcrHome() {
  return join(dirname(dataRoot()), 'oar-ocr', oarOcrVersion);
}

async function exists(path) {
  return stat(path).then(() => true, (error) => {
    if (error.code === 'ENOENT') return false;
    throw error;
  });
}

async function extract(archive, target) {
  const tar = process.platform === 'win32' && process.env.SystemRoot ? join(process.env.SystemRoot, 'System32', 'tar.exe') : 'tar';
  const child = spawn(tar, ['-xf', archive, '-C', target], { stdio: ['ignore', 2, 2], windowsHide: true });
  const [code, signal] = await once(child, 'exit');
  if (code !== 0) throw new Error(`Could not extract ${archive} (${signal ?? code}).`);
}

/** The ONNX Runtime library in an extracted release: the file itself, not a link to it. */
async function runtimeLibrary(lib) {
  const names = await readdir(lib);
  const pattern = process.platform === 'win32' ? /^onnxruntime\.dll$/u
    : process.platform === 'darwin' ? /^libonnxruntime\.\d+\.\d+\.\d+\.dylib$/u : /^libonnxruntime\.so\.\d+\.\d+\.\d+$/u;
  const name = names.find((entry) => pattern.test(entry));
  if (!name) throw new Error('The ONNX Runtime release has no library for this platform.');
  return join(lib, name);
}

/** Downloads and checks ONNX Runtime and the models once. Returns the prepared folder. */
export async function setupOarOcr() {
  const platform = `${process.platform}/${process.arch}`;
  const runtime = runtimes[platform];
  if (!runtime) throw Object.assign(new Error(`oar-ocr is not available for ${platform}.`), { code: 'OCR_NOT_PREPARED' });
  const home = oarOcrHome();
  const manifest = join(home, 'oar-ocr.json');
  if (await exists(manifest)) return home;
  await mkdir(home, { recursive: true });
  for (const [file, checksum] of Object.values(models)) await downloadVerified(`${modelRelease}/${file}`, join(home, file), checksum, 600_000);
  const [name, format, checksum] = runtime;
  const folder = `${name}-${runtimeVersion}`;
  const archive = join(home, `${folder}.${format}`);
  await downloadVerified(`${runtimeRelease}/${folder}.${format}`, archive, checksum, 600_000);
  const staging = join(home, 'runtime.tmp');
  await rm(staging, { recursive: true, force: true });
  await mkdir(staging);
  try {
    await extract(archive, staging);
    await rm(join(home, 'runtime'), { recursive: true, force: true });
    await rename(join(staging, folder), join(home, 'runtime'));
  } finally {
    await rm(staging, { recursive: true, force: true });
    await rm(archive, { force: true });
  }
  const library = await runtimeLibrary(join(home, 'runtime', 'lib'));
  const files = Object.fromEntries(Object.entries(models).map(([role, [file]]) => [role, file]));
  // Written last: its presence means the folder is complete.
  await writeFile(manifest, JSON.stringify({ version: oarOcrVersion, runtime: relative(home, library).split('\\').join('/'), ...files }, null, 2) + '\n');
  return home;
}
