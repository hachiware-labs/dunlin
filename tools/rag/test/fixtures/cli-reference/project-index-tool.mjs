import { spawn } from 'node:child_process';
import { chmod, lstat, mkdir, readFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { atomicWrite, revision } from './authoring/files.mjs';

const protocol = 1;
const toolError = (code, message, exitCode = 2) => Object.assign(new Error(message), { code, exitCode });
const platform = process.platform + '-' + process.arch;
const suffix = process.platform === 'win32' ? '.exe' : '';
const bundled = fileURLToPath(new URL('../../../../../dist/tools/', import.meta.url));
const require = createRequire(import.meta.url);
export const hannoy = {
  name: 'hannoy', version: '0.2.0', modules: ['hannoy.index', 'hannoy.search'],
  configuration: {
    index: { use: 'hannoy.index', with: { model: 'local-embed', ef_construction: 100 } },
    search: { use: 'hannoy.search', with: { model: 'local-embed', top_k: 10, ef_search: 100 } },
  },
};

function binaryName(tool) {
  return 'dunlin-' + tool.name + '-' + platform + suffix;
}

async function manifestAt(directory, tool) {
  const name = binaryName(tool);
  const manifest = JSON.parse(await readFile(join(directory, name + '.json'), 'utf8'));
  if (manifest.version !== tool.version || manifest.protocol !== protocol || manifest.file !== name
    || !/^[0-9a-f]{64}$/u.test(manifest.sha256)) throw toolError('TOOL_INTEGRITY', 'The packaged index tool manifest is invalid.');
  return manifest;
}

async function shippedTool(tool) {
  try { return { directory: bundled, manifest: await manifestAt(bundled, tool) }; }
  catch (error) { if (error.code !== 'ENOENT') throw error; }
  const abi = { win32: '-msvc', linux: '-gnu', darwin: '' }[process.platform];
  let native;
  if (abi !== undefined) {
    try { native = require.resolve('dunlin-' + platform + abi); }
    catch (error) { if (error.code !== 'MODULE_NOT_FOUND') throw error; }
  }
  if (!native) throw toolError('TOOL_REQUIRED', 'The package has no ' + tool.name + ' for this platform.', 3);
  const directory = join(dirname(native), 'tools');
  return { directory, manifest: await manifestAt(directory, tool) };
}

async function verified(directory, tool, packaged = false) {
  const name = binaryName(tool);
  try {
    const shipped = await shippedTool(tool);
    const { manifest } = shipped;
    const path = join(packaged ? shipped.directory : directory, name);
    const info = await lstat(path);
    if (!info.isFile()) throw new Error();
    const bytes = await readFile(path);
    if (revision(bytes) !== manifest.sha256) throw new Error();
    return { path, manifest, bytes };
  } catch (error) {
    if (error.exitCode) throw error;
    if (error.code === 'ENOENT') throw toolError('TOOL_REQUIRED', 'The ' + tool.name + ' binary is missing. Run dunlin add ' + tool.name + '.', 3);
    throw toolError('TOOL_INTEGRITY', 'The ' + tool.name + ' binary failed its version or integrity check.');
  }
}

export async function installIndexTool(project, tool) {
  const supplied = await verified(null, tool, true);
  const dir = join(project.paths.data, 'tools', tool.name, tool.version, platform);
  await mkdir(dir, { recursive: true });
  const path = join(dir, supplied.manifest.file);
  await atomicWrite(path, supplied.bytes);
  await chmod(path, 0o755);
  const executable = await openIndexTool(project.paths, tool);
  const description = await executable.describe();
  if (description.protocol !== protocol || description.name !== tool.name || description.version !== tool.version
    || JSON.stringify(description.modules) !== JSON.stringify(tool.modules)
    || !['build', 'search'].every((value) => description.capabilities?.includes(value))) {
    throw toolError('TOOL_PROTOCOL', 'The installed index tool uses an incompatible protocol.');
  }
}

export async function checkIndexTool(paths, tool) {
  await verified(join(paths.data, 'tools', tool.name, tool.version, platform), tool);
}

/** The path of the installed tool, after its version and integrity check. */
export async function indexToolPath(paths, tool) {
  return (await verified(join(paths.data, 'tools', tool.name, tool.version, platform), tool)).path;
}

export async function openIndexTool(paths, tool, signal) {
  const { path } = await verified(join(paths.data, 'tools', tool.name, tool.version, platform), tool);
  const execute = (operation, request = {}) => call(path, { operation, ...request }, signal);
  return { describe: () => execute('describe'), build: (request) => execute('build', request), search: (request) => execute('search', request) };
}

function call(binary, request, signal) {
  signal?.throwIfAborted();
  return new Promise((resolve, reject) => {
    const env = Object.fromEntries(['SystemRoot', 'WINDIR', 'TMP', 'TEMP', 'TMPDIR'].flatMap((key) =>
      process.env[key] === undefined ? [] : [[key, process.env[key]]]));
    const child = spawn(binary, { windowsHide: true, stdio: ['pipe', 'pipe', 'ignore'], env, signal });
    let output = '';
    child.stdout.setEncoding('utf8');
    child.stdout.on('data', (chunk) => {
      output += chunk;
      if (output.length > 1024 * 1024) {
        child.kill();
        reject(toolError('TOOL_PROTOCOL', 'The index tool response exceeded its size limit.', 1));
      }
    });
    child.on('error', (error) => reject(signal?.aborted ? signal.reason : toolError('TOOL_EXECUTION', 'Could not start the index tool.', 1)));
    child.stdin.on('error', () => {});
    child.on('close', (code) => {
      if (code !== 0) {
        reject(signal?.aborted ? signal.reason : toolError('TOOL_EXECUTION', 'The index tool failed during ' + request.operation + '.', 1));
        return;
      }
      try { resolve(JSON.parse(output)); }
      catch { reject(toolError('TOOL_PROTOCOL', 'The index tool returned invalid JSON.', 1)); }
    });
    child.stdin.end(JSON.stringify(request));
  });
}
