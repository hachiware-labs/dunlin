import { open, opendir, realpath, stat } from 'node:fs/promises';
import { basename, extname, join, relative, sep } from 'node:path';
import { pathToFileURL } from 'node:url';

import { prepareDocument } from './authoring/uploads.mjs';
import { parseConfigured, parserCategory } from './parsers/parse.mjs';

const textLimit = 1024 * 1024;
export const convertedLimit = 16 * textLimit;
const textExtensions = new Set(['.md', '.markdown', '.txt', '.rs', '.js', '.mjs', '.cjs', '.ts', '.tsx', '.jsx',
  '.py', '.go', '.java', '.cs', '.json', '.toml', '.yaml', '.yml', '.sql']);
const htmlExtensions = new Set(['.html', '.htm']);

export async function ingestFiles(database, { sourceId, uploadRoot, parsers, keepOriginal }, location) {
  const path = await realpath(location);
  const info = await stat(path);
  if (info.isFile()) {
    const name = basename(path);
    const { input, original } = await prepareLocalFile(uploadRoot, sourceId, path, name, parsers, keepOriginal);
    let report;
    if (parserCategory(parsers, name)) {
      await database.syncDocuments(sourceId, [input]);
    } else {
      report = await database.replaceDocuments(sourceId, [input]);
    }
    const { evidence } = await database.getDocument({ sourceId, objectId: input.objectId });
    return { ...report, document: evidence.source, totalLines: evidence.range.endLine, ...(original && { original }) };
  }
  if (!info.isDirectory()) throw new Error('Expected a local file or directory');
  const documents = [];
  for await (const file of listFiles(path)) {
    const name = basename(file);
    const extension = extname(name).toLowerCase();
    if (!textExtensions.has(extension) && !htmlExtensions.has(extension) && !parserCategory(parsers, name)) continue;
    const id = relative(path, file).split(sep).join('/');
    const { input } = await prepareLocalFile(uploadRoot, sourceId, file, id, parsers, keepOriginal);
    documents.push(input);
  }
  return database.replaceDocuments(sourceId, documents);
}

export async function prepareLocalFile(uploadRoot, sourceId, path, objectId, parsers, keepOriginal, signal) {
  const name = basename(path);
  const extension = extname(name).toLowerCase();
  const category = parserCategory(parsers, name);
  const html = htmlExtensions.has(extension);
  if (!category && !html && !textExtensions.has(extension)) throw new Error('Expected a supported UTF-8 file or a configured parser');
  const file = await open(path, 'r');
  let bytes;
  let modifiedAtMs;
  try {
    const before = await file.stat();
    if (!before.isFile()) throw new Error('Expected a local file');
    const limit = category ? convertedLimit : html ? 4 * textLimit : textLimit;
    bytes = await readBounded(file.createReadStream({ autoClose: false }), limit, signal);
    const after = await stat(path);
    if (before.mtimeMs !== after.mtimeMs || before.size !== after.size) throw new Error('A source file changed while it was being indexed');
    if (before.mtimeMs >= 0) modifiedAtMs = Math.trunc(before.mtimeMs);
  } finally {
    await file.close();
  }
  let parsed;
  if (category || html) {
    parsed = await decodeRemote([bytes], undefined, name, { parsers, keepOriginal, signal });
  } else {
    const content = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(bytes);
    parsed = { content, format: ['.md', '.markdown'].includes(extension) ? 'markdown' : 'text', ...(keepOriginal && { bytes }) };
  }
  signal?.throwIfAborted();
  return prepareDocument(uploadRoot, sourceId, { objectId, url: pathToFileURL(path).href, modifiedAtMs }, parsed);
}

export async function decodeRemote(body, type, name, { parsers, keepOriginal, signal } = {}) {
  const category = parserCategory(parsers, name, type);
  if (category) {
    const bytes = await readBounded(body, convertedLimit, signal);
    const parsed = await parseConfigured(parsers, bytes, basename(name) || 'document', type, signal);
    return { ...parsed, ...((keepOriginal || category !== 'code') && { bytes }) };
  }
  const { readDocument } = await import('../import/import-documents.mjs');
  return readDocument(body, type, name, { keepOriginal });
}

export async function readBounded(body, limit, signal) {
  if (!body) throw new Error('Expected a document body');
  const chunks = [];
  let length = 0;
  for await (const chunk of body) {
    signal?.throwIfAborted();
    length += chunk.byteLength;
    if (length > limit) throw new Error(`Source file exceeds ${limit / textLimit} MiB`);
    chunks.push(chunk);
  }
  return Buffer.concat(chunks, length);
}

async function* listFiles(path) {
  const directory = await opendir(path);
  for await (const entry of directory) {
    if (entry.name.startsWith('.') || entry.name === 'node_modules' || entry.isSymbolicLink()) continue;
    const file = join(path, entry.name);
    if (entry.isDirectory()) yield* listFiles(file);
    else if (entry.isFile()) yield file;
  }
}
