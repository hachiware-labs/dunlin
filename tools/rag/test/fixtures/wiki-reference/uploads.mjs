import { readdir } from 'node:fs/promises';
import { join } from 'node:path';

import { atomicWrite, directory, readBytes, readText, revision, withWriteLock } from './files.mjs';

const uploadedSourceId = /^upload-[a-f0-9]{57}$/u;
const sha256Pattern = /^[a-f0-9]{64}$/u;
const sharedWrites = new Map();
export const originalUriTemplate = 'knowledge-nest://original/{sourceId}/{sha256}';
export const originalMimeType = 'application/octet-stream';

export async function withUploadLock(root, document, write) {
  await directory(root, true);
  const parent = uploadDirectory(root, archiveId(document));
  await directory(parent, true);
  return withWriteLock(parent, write);
}

export async function prepareDocument(root, sourceId, document, parsed, { sharedOriginals = false } = {}) {
  const { bytes, content, format, locations, parser } = parsed;
  const original = bytes && await saveOriginal(root, { sourceId, ...document }, bytes, sharedOriginals);
  return {
    input: { ...document, content, format, corpus: { locations, parser, originalSha256: original?.sha256 } },
    original,
  };
}

async function saveOriginal(root, document, bytes, shared) {
  const sourceId = shared ? 'shared' : archiveId(document);
  const parent = shared ? join(root, 'shared') : uploadDirectory(root, sourceId);
  await directory(root, true);
  await directory(parent, true);
  const sha256 = revision(bytes);
  const path = join(parent, `${sha256}.bin`);
  if (shared) {
    let pending = sharedWrites.get(path);
    if (!pending) {
      pending = { bytes, done: writeOriginal(path, bytes).finally(() => sharedWrites.delete(path)) };
      sharedWrites.set(path, pending);
    } else if (!pending.bytes.equals(bytes)) {
      throw new Error('Stored original file does not match its SHA-256 digest');
    }
    await pending.done;
  } else {
    await writeOriginal(path, bytes);
  }
  return { ...originalForDigest(document, sha256), size: bytes.length };
}

async function writeOriginal(path, bytes) {
  const existing = await readBytes(path);
  if (existing === null) {
    await atomicWrite(path, bytes);
  } else if (!existing.equals(bytes)) {
    throw new Error('Stored original file does not match its SHA-256 digest');
  }
}

export function originalForDigest(document, sha256) {
  return originalReference(archiveId(document), sha256, originalName(document.objectId));
}

export async function sharedOriginalForDigest(root, document, sha256) {
  if (!sha256Pattern.test(sha256)) throw new Error('Invalid original digest');
  const bytesPath = join(root, 'shared', sha256 + '.bin');
  const { stat } = await import('node:fs/promises');
  const info = await stat(bytesPath).catch((error) => { if (error.code !== 'ENOENT') throw error; });
  return info?.isFile() ? originalReference('shared', sha256, originalName(document.objectId)) : originalForDigest(document, sha256);
}

export function isUploadedSource(sourceId) {
  return uploadedSourceId.test(sourceId);
}

export async function originalForContent(root, document, indexedContent, requestedDigest) {
  const sourceId = archiveId(document);
  const parent = uploadDirectory(root, sourceId);
  const digests = await directory(root) && await directory(parent)
    ? await readDigests(associationPath(parent, indexedContent)) : [];
  if (requestedDigest !== undefined && !digests.includes(requestedDigest)) {
    throw new Error('Original file does not match the cited source');
  }
  const sha256 = requestedDigest ?? digests.at(-1);
  return sha256 ? originalReference(sourceId, sha256, originalName(document.objectId)) : null;
}

export async function listOriginals(root, document) {
  if (!await directory(root)) return [];
  const sourceId = archiveId(document);
  const parent = uploadDirectory(root, sourceId);
  if (!await directory(parent)) return [];
  const entries = await readdir(parent, { withFileTypes: true });
  return entries.filter((entry) => entry.isFile() && /^[a-f0-9]{64}\.bin$/u.test(entry.name))
    .map(({ name }) => originalReference(sourceId, name.slice(0, -4), originalName(document.objectId)));
}

export async function citedLocations(root, document, content, original, range) {
  const parent = uploadDirectory(root, archiveId(document));
  const data = await readText(locationsPath(parent, content, original.sha256));
  if (data === null) return [];
  return JSON.parse(data).filter(({ range: block }) => block.startLine <= range.endLine && range.startLine <= block.endLine);
}

export async function readOriginal(root, sourceId, sha256) {
  const parent = uploadDirectory(root, sourceId);
  if (typeof sha256 !== 'string' || !sha256Pattern.test(sha256)) throw new Error('Invalid original file digest');
  if (!await directory(root) || !await directory(parent)) throw new Error('Original file not found');
  const bytes = await readBytes(join(parent, `${sha256}.bin`));
  if (bytes === null) throw new Error('Original file not found');
  if (revision(bytes) !== sha256) throw new Error('Stored original file does not match its SHA-256 digest');
  return bytes;
}

function uploadDirectory(root, sourceId) {
  if (typeof sourceId !== 'string' || !uploadedSourceId.test(sourceId)) throw new Error('Invalid uploaded source ID');
  return join(root, sourceId);
}

function associationPath(parent, content) {
  // Different original files can yield the same indexed text, for example after BOM removal.
  return join(parent, `content-${revision(content)}.json`);
}

function locationsPath(parent, content, sha256) {
  return join(parent, `locations-${revision(content)}-${sha256}.json`);
}

function archiveId({ sourceId, objectId }) {
  return isUploadedSource(sourceId) ? sourceId : `upload-${revision(JSON.stringify([sourceId, objectId])).slice(0, 57)}`;
}

function originalName(objectId) {
  const url = /^https?:\/\//iu.test(objectId) ? URL.parse(objectId) : null;
  const name = url?.pathname.split('/').at(-1) || objectId;
  return name.replace(/[/\\\x00-\x1f\x7f]/gu, '_');
}

async function readDigests(path) {
  const content = await readText(path);
  if (content === null) return [];
  const digests = JSON.parse(content);
  if (!Array.isArray(digests) || digests.some((digest) => typeof digest !== 'string' || !sha256Pattern.test(digest))) {
    throw new Error('Invalid original file association');
  }
  return digests;
}

function originalReference(sourceId, sha256, name) {
  return { name, sha256, uri: `knowledge-nest://original/${sourceId}/${sha256}` };
}
