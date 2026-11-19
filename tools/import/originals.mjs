// The originals that `--keep-original` saves next to an index, in the layout the CLI's uploads used
// (`.dunlin-uploads-v1/upload-<id>/<sha256>.bin`). This tool kept its own copy when the CLI moved
// to Rust (spec 28.13).
import { createHash, randomUUID } from 'node:crypto';
import { lstat, mkdir, readFile, rename, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';

const uploadedSourceId = /^upload-[a-f0-9]{57}$/u;
const sha256Pattern = /^[a-f0-9]{64}$/u;

function revision(content) {
  return createHash('sha256').update(content).digest('hex');
}

async function atomicWrite(path, content) {
  const temporary = `${path}.${randomUUID()}.tmp`;
  try {
    await writeFile(temporary, content, { flag: 'wx' });
    await rename(temporary, path);
  } finally {
    await rm(temporary, { force: true });
  }
}

/** The bytes of a regular file, or null when it does not exist. */
async function readBytes(path) {
  const entry = await lstat(path).catch((error) => { if (error.code !== 'ENOENT') throw error; });
  if (!entry) return null;
  if (!entry.isFile() || entry.isSymbolicLink()) throw new Error(`Expected a regular file at ${path}`);
  return readFile(path);
}

function archiveId({ sourceId, objectId }) {
  return uploadedSourceId.test(sourceId) ? sourceId : `upload-${revision(JSON.stringify([sourceId, objectId])).slice(0, 57)}`;
}

function uploadDirectory(root, sourceId) {
  if (typeof sourceId !== 'string' || !uploadedSourceId.test(sourceId)) throw new Error('Invalid uploaded source ID');
  return join(root, sourceId);
}

function originalName(objectId) {
  const url = /^https?:\/\//iu.test(objectId) ? URL.parse(objectId) : null;
  const name = url?.pathname.split('/').at(-1) || objectId;
  return name.replace(/[/\\\x00-\x1f\x7f]/gu, '_');
}

/** `{ name, sha256, uri }` of a saved original: `dunlin://original/<archive>/<sha256>`. */
export function originalForDigest(document, sha256) {
  return { name: originalName(document.objectId), sha256, uri: `dunlin://original/${archiveId(document)}/${sha256}` };
}

/**
 * The input of `replaceDocuments` for a parsed document. With `bytes`, saves the original under
 * `root` and records its SHA-256 in the corpus.
 */
export async function prepareDocument(root, sourceId, document, parsed) {
  const { bytes, content, format, locations, parser } = parsed;
  let original;
  if (bytes) {
    const parent = uploadDirectory(root, archiveId({ sourceId, ...document }));
    await mkdir(parent, { recursive: true });
    const sha256 = revision(bytes);
    const path = join(parent, `${sha256}.bin`);
    const existing = await readBytes(path);
    if (existing === null) await atomicWrite(path, bytes);
    else if (!existing.equals(bytes)) throw new Error('Stored original file does not match its SHA-256 digest');
    original = { ...originalForDigest({ sourceId, ...document }, sha256), size: bytes.length };
  }
  return {
    input: { ...document, content, format, corpus: { locations, parser, originalSha256: original?.sha256 } },
    original,
  };
}

/** The bytes of a saved original, checked against its digest. */
export async function readOriginal(root, sourceId, sha256) {
  const parent = uploadDirectory(root, sourceId);
  if (typeof sha256 !== 'string' || !sha256Pattern.test(sha256)) throw new Error('Invalid original file digest');
  const bytes = await readBytes(join(parent, `${sha256}.bin`));
  if (bytes === null) throw new Error('Original file not found');
  if (revision(bytes) !== sha256) throw new Error('Stored original file does not match its SHA-256 digest');
  return bytes;
}
