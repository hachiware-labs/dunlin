import { createHash, randomUUID } from 'node:crypto';
import { lstat, mkdir, open, readFile, realpath, rename, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';

export const categories = ['Projects', 'Areas', 'Resources', 'Archives'];

export async function prepareRoot(path) {
  await mkdir(path, { recursive: true });
  return realpath(path);
}

export async function directory(path, create = false) {
  if (create) await mkdir(path).catch((error) => { if (error.code !== 'EEXIST') throw error; });
  const entry = await statIfExists(path);
  if (!entry) return false;
  if (!entry.isDirectory() || entry.isSymbolicLink()) throw new Error(`Expected a regular directory at ${path}`);
  return true;
}

export function idParts(id) {
  const parts = id.split('/');
  if (id.length > 240 || parts.length > 8 || parts.some((part) => !part || part.startsWith('.')
    || /[<>:"\\|?*\x00-\x1f]/u.test(part) || /[. ]$/u.test(part)
    || /^(?:con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)/iu.test(part))) {
    throw new Error('Expected a safe relative page ID');
  }
  return parts;
}

export async function pageFile(root, id, extension, create = false) {
  const parts = idParts(id);
  const file = `${parts.pop()}.${extension}`;
  let parent = root;
  for (const part of parts) {
    parent = join(parent, part);
    if (!await directory(parent, create)) return null;
  }
  return join(parent, file);
}

export async function readText(path) {
  const bytes = await readBytes(path);
  return bytes === null ? null : bytes.toString('utf8');
}

export async function readBytes(path) {
  if (!path) return null;
  const entry = await statIfExists(path);
  if (!entry) return null;
  if (!entry.isFile() || entry.isSymbolicLink()) throw new Error(`Expected a regular file at ${path}`);
  return readFile(path);
}

async function statIfExists(path) {
  return lstat(path).catch((error) => { if (error.code !== 'ENOENT') throw error; });
}

export async function atomicWrite(path, content) {
  const temporary = `${path}.${randomUUID()}.tmp`;
  try {
    await writeFile(temporary, content, { flag: 'wx' });
    await rename(temporary, path);
  } finally {
    await rm(temporary, { force: true });
  }
}

export async function withWriteLock(root, write) {
  const path = join(root, '.dunlin-write.lock');
  const lock = await open(path, 'wx').catch((error) => {
    if (error.code !== 'EEXIST') throw error;
    throw new Error(`Another writer is updating ${root}`);
  });
  try {
    return await write();
  } finally {
    await lock.close();
    await rm(path, { force: true });
  }
}

export function revision(content) {
  return createHash('sha256').update(content).digest('hex');
}

export function checkPageRevision(id, content, expected) {
  if ((content === null ? null : revision(content)) !== expected) {
    throw new Error(`Revision conflict for ${id}. Read the latest page before saving.`);
  }
}

export async function checkSourceRevision(database, expected, signal) {
  if (await database.getIndexRevision({ signal }) !== expected) {
    throw new Error('Sources changed during this update. Read the current sources and retry.');
  }
}

const sourcesMarker = '\n\n<!-- dunlin:sources -->\n';
const sourcesEnd = '<!-- /dunlin:sources -->\n';

export function citationLocations(locations = []) {
  const labels = locations.map(({ page, symbol }) => page ? `p. ${page}` : symbol).filter(Boolean);
  return [...new Set(labels)].join(', ');
}

export function withoutSources(markdown) {
  const start = markdown.lastIndexOf(sourcesMarker);
  return start >= 0 && markdown.endsWith(sourcesEnd) ? markdown.slice(0, start) : markdown;
}
