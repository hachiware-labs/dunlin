import { execFile, spawn } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, mkdtemp, readFile, readdir, rename, rm, stat, writeFile } from 'node:fs/promises';
import { homedir } from 'node:os';
import { basename, isAbsolute, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { lightonModel } from './catalog.mjs';
import { downloadVerified } from './download.mjs';

export const lightonServer = fileURLToPath(new URL('./lighton-server.py', import.meta.url));
const revision = '23d544085c886bbdace647bcfab91203299417e2';
export const managedOcrVersion = revision;
const modelFiles = {
  'chat_template.jinja': [720, '0942a43ee0ba48a608814e0b40ac9cb70bcc29ea72f19c84b1bf2597f05aa2bd'],
  'config.json': [2363, '43b62f9ec0a7207f67fd032f7ef9719fa8b6b44d7b3cf20fbc2346329493ff3a'],
  'generation_config.json': [219, '7d2566b271454af41e9ebc28851888e1ec7ca33a53fe644dac207856df952981'],
  'model.safetensors': [2011367464, '32879861d5cdc3b2c3909d3f9a47dc8f7338685d13113c8c98c144f68b358dbb'],
  'processor_config.json': [1023, '5292a61a089835ec6884836e3896bba1fc571c60df872e3ca9eb45bd3288910d'],
  'special_tokens_map.json': [613, '76862e765266b85aa9459767e33cbaf13970f327a0e88d1c65846c2ddd3a1ecd'],
  'tokenizer_config.json': [5591, '76d82c8010d0e0d5431aa5e3658c69e11a2a7250e789799d2be4ab75dc3fafed'],
  'tokenizer.json': [11422822, 'f54b55fa0c3aba0c91ce09ea79ae4e62da24e2a1a630f96c4bae34aba25e234a'],
};
const uvRelease = 'https://releases.astral.sh/github/uv/releases/download/0.13.7';
const uvAssets = {
  'win32/x64': ['uv-x86_64-pc-windows-msvc.zip', '0cdc5b38fe5a8168ed6a25814f5ba43c21d313dd3187314fccb37b7cec46e021'],
  'linux/x64': ['uv-x86_64-unknown-linux-gnu.tar.gz', 'c5221ff41b7892bfc223543883600125f6a47fe845093e0f5bdff4f359196d83'],
  'darwin/arm64': ['uv-aarch64-apple-darwin.tar.gz', '81b811b57e4624df7ed3aac148e0359100d9962f3d9dce00aec533dc2e086731'],
};
const pythonRelease = 'https://github.com/astral-sh/python-build-standalone/releases/download/20261026';
const pythonAssets = {
  'win32/x64': ['x86_64-pc-windows-msvc', '1f2ab53880fc67ea323398c6388e05af28addc99a8e389209f08a51fdff4b149'],
  'linux/x64': ['x86_64-unknown-linux-gnu', 'ce4f6d0a6cfdb3bed6e44c2f9affd85943c76a42e25ad3a259002f31d6fb8a20'],
  'darwin/arm64': ['aarch64-apple-darwin', '353d5f1b4fd2715e7053f12f068435d5bbad16b91e057006b5d01e0925709a7e'],
};
const windows = process.platform === 'win32';
const platform = `${process.platform}/${process.arch}`;
const backends = platform === 'darwin/arm64' ? ['mps'] : ['cuda', 'rocm'];
const exec = promisify(execFile);
const torchPackages = ['torch==2.9.1', 'torchvision==0.24.1'];
const rocmWindows = 'https://repo.radeon.com/rocm/windows/rocm-rel-7.2.1';
const rocmLinux = 'https://repo.radeon.com/rocm/manylinux/rocm-rel-7.2.1';

export function dataRoot() {
  const override = process.env.DUNLIN_OCR_HOME;
  if (override !== undefined) {
    if (!isAbsolute(override)) throw new Error('DUNLIN_OCR_HOME must be an absolute path');
    return override;
  }
  if (windows) {
    const local = process.env.LOCALAPPDATA;
    return join(local && isAbsolute(local) ? local : join(homedir(), 'AppData', 'Local'), 'Dunlin', 'ocr');
  }
  if (process.platform === 'darwin') return join(homedir(), 'Library', 'Application Support', 'Dunlin', 'ocr');
  const xdg = process.env.XDG_DATA_HOME;
  return join(xdg && isAbsolute(xdg) ? xdg : join(homedir(), '.local', 'share'), 'dunlin', 'ocr');
}

function pythonPath(runtime) {
  return join(runtime, 'venv', ...(windows ? ['Scripts', 'python.exe'] : ['bin', 'python']));
}

function modelPath(root) {
  return join(root, `model-${revision}`);
}

async function installedRuntime(root) {
  try {
    const value = JSON.parse(await readFile(join(root, 'runtime.json'), 'utf8'));
    if (value?.version !== 1 || value.platform !== platform || !backends.includes(value.backend)
      || typeof value.directory !== 'string' || !/^rt-[a-z0-9]{6}$/iu.test(value.directory)) return;
    const python = pythonPath(join(root, value.directory));
    if (!(await stat(python)).isFile()) return;
    return { ...value, python };
  } catch (error) {
    if (error.code !== 'ENOENT' && !(error instanceof SyntaxError)) throw error;
  }
}

export async function managedOcr({ required = true, requireModel = true } = {}) {
  const root = dataRoot();
  const runtime = await installedRuntime(root);
  const modelDir = modelPath(root);
  const files = runtime && requireModel && await Promise.all(Object.entries(modelFiles).map(async ([name, [size]]) => {
    try {
      const file = await stat(join(modelDir, name));
      return file.isFile() && file.size === size;
    } catch (error) {
      if (error.code !== 'ENOENT') throw error;
      return false;
    }
  }));
  if (!runtime || (files && files.some((present) => !present))) {
    if (required) {
      throw Object.assign(new Error('Local GPU OCR is not prepared. Run dunlin add ocr.'), { code: 'OCR_NOT_PREPARED' });
    }
    return;
  }
  return { python: runtime.python, modelDir, backend: runtime.backend };
}

export async function probeGpu(python, backend) {
  try {
    const { stdout } = await exec(python, ['-I', lightonServer, '--probe'], { windowsHide: true, timeout: 60_000 });
    const { device, deviceName, runtime, dtype } = JSON.parse(stdout);
    if (runtime.toLowerCase() !== backend) throw new Error(`Expected ${backend}, got ${runtime}`);
    return { device, deviceName, runtime, dtype };
  } catch (error) {
    const detail = error.stderr?.trim().split(/\r?\n/u).at(-1) || error.message;
    throw new Error(`GPU check failed for ${backend}: ${detail}. Check the vendor driver and GPU compatibility.`, { cause: error });
  }
}

async function detectBackend() {
  if (process.platform === 'darwin') return 'mps';
  if (windows) {
    const { stdout } = await exec('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', '(Get-CimInstance Win32_VideoController).PNPDeviceID'], { windowsHide: true, timeout: 15_000 });
    if (/VEN_10DE/iu.test(stdout)) return 'cuda';
    if (/VEN_1002/iu.test(stdout)) return 'rocm';
  } else {
    const cards = await readdir('/sys/class/drm').catch(() => []);
    const vendors = await Promise.all(cards.filter((card) => /^card\d+$/u.test(card))
      .map((card) => readFile(`/sys/class/drm/${card}/device/vendor`, 'utf8').catch(() => '')));
    if (vendors.some((vendor) => vendor.trim() === '0x10de')) return 'cuda';
    const nvidia = await exec('nvidia-smi', ['-L'], { timeout: 5000 }).catch(() => null);
    if (nvidia?.stdout.includes('GPU ')) return 'cuda';
    if (vendors.some((vendor) => vendor.trim() === '0x1002')) return 'rocm';
  }
  throw new Error('Could not identify an NVIDIA or AMD GPU. Check the vendor driver or specify --backend cuda or --backend rocm.');
}

async function run(executable, args) {
  const child = spawn(executable, args, { stdio: ['ignore', 2, 2], windowsHide: true });
  const [code, signal] = await once(child, 'exit');
  if (code !== 0) throw new Error(`${basename(executable)} ${args[0]} failed (${signal ?? code}). See setup output above.`);
}

async function extract(release, asset, checksum, runtime) {
  const archive = join(runtime, asset);
  await downloadVerified(`${release}/${encodeURIComponent(asset)}`, archive, checksum);
  const tar = windows && process.env.SystemRoot ? join(process.env.SystemRoot, 'System32', 'tar.exe') : 'tar';
  await run(tar, ['-xf', archive, '-C', runtime]);
  await rm(archive);
}

async function installRuntime(runtime, backend) {
  const [asset, uvChecksum] = uvAssets[platform];
  const [target, pythonChecksum] = pythonAssets[platform];
  await extract(uvRelease, asset, uvChecksum, runtime);
  await extract(pythonRelease, `cpython-3.12.16+20261026-${target}-install_only_stripped.tar.gz`, pythonChecksum, runtime);
  const uv = windows ? join(runtime, 'uv.exe') : join(runtime, asset.replace(/\.tar\.gz$/u, ''), 'uv');
  const basePython = join(runtime, 'python', ...(windows ? ['python.exe'] : ['bin', 'python3.12']));
  const python = pythonPath(runtime);
  await run(uv, ['venv', '--no-config', '--no-python-downloads', '--python', basePython, join(runtime, 'venv')]);
  const pip = (...args) => run(uv, ['pip', 'install', '--no-config', '--python', python, ...args]);
  if (backend === 'rocm' && windows) {
    await pip(...['rocm_sdk_core-7.2.1-py3-none-win_amd64.whl', 'rocm_sdk_devel-7.2.1-py3-none-win_amd64.whl',
      'rocm_sdk_libraries_custom-7.2.1-py3-none-win_amd64.whl', 'rocm-7.2.1.tar.gz',
      'torch-2.9.1%2Brocm7.2.1-cp312-cp312-win_amd64.whl', 'torchvision-0.24.1%2Brocm7.2.1-cp312-cp312-win_amd64.whl']
      .map((file) => `${rocmWindows}/${file}`));
  } else if (backend === 'rocm') {
    await pip(...['torch-2.9.1%2Brocm7.2.1.lw.gitff65f5bc-cp312-cp312-linux_x86_64.whl',
      'torchvision-0.24.0%2Brocm7.2.1.gitb919bd0c-cp312-cp312-linux_x86_64.whl',
      'triton-3.5.1%2Brocm7.2.1.gita272dfa8-cp312-cp312-linux_x86_64.whl'].map((file) => `${rocmLinux}/${file}`));
  } else if (backend === 'cuda') {
    await pip('--index-url', 'https://download.pytorch.org/whl/cu128', ...torchPackages);
  } else {
    await pip(...torchPackages);
  }
  await pip('transformers==5.16.1', 'Pillow');
  return python;
}

export async function setupManagedOcr(requestedBackend) {
  if (!Object.hasOwn(uvAssets, platform)) throw new Error(`Managed GPU OCR is unavailable on ${platform}. Use oar-ocr instead: dunlin add oar-ocr.`);
  if (requestedBackend !== undefined && !backends.includes(requestedBackend)) {
    throw new Error(`Expected --backend ${backends.join(' or ')} on ${platform}`);
  }
  const root = dataRoot();
  const existing = await installedRuntime(root);
  const reusable = existing && (requestedBackend === undefined || existing.backend === requestedBackend);
  let info = reusable ? await probeGpu(existing.python, existing.backend).catch(() => undefined) : undefined;
  const backend = requestedBackend ?? (info ? existing.backend : await detectBackend());
  await mkdir(root, { recursive: true });
  const runtime = info ? undefined : await mkdtemp(join(root, 'rt-'));
  const temporary = join(root, `runtime-${randomUUID()}.tmp`);
  let published = false;
  try {
    if (runtime) {
      console.error(`Preparing the ${backend} OCR runtime in ${root}`);
      info = await probeGpu(await installRuntime(runtime, backend), backend);
    }
    console.error(`Preparing and verifying ${lightonModel} in ${modelPath(root)}`);
    const modelUrl = `https://huggingface.co/${lightonModel}/resolve/${revision}`;
    const downloads = await Promise.allSettled(Object.entries(modelFiles).map(([name, [, checksum]]) =>
      downloadVerified(`${modelUrl}/${name}`, join(modelPath(root), name), checksum, 7_200_000)));
    const failure = downloads.find(({ status }) => status === 'rejected');
    if (failure) throw failure.reason;
    if (runtime) {
      await writeFile(temporary, JSON.stringify({ version: 1, platform, backend, directory: basename(runtime) }), { flag: 'wx' });
      await rename(temporary, join(root, 'runtime.json'));
      published = true;
    }
    return info;
  } finally {
    await rm(temporary, { force: true });
    if (runtime && !published) await rm(runtime, { recursive: true, force: true, maxRetries: 3 });
  }
}
