import { createHash } from 'node:crypto';
import { realpath } from 'node:fs/promises';
import { basename, isAbsolute, relative, resolve, sep } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import { parseConfigured, parserCategory } from '../parsers/parse.mjs';
import { convertedLimit, decodeRemote, prepareLocalFile } from '../ingest-files.mjs';
import { citedLocations, listOriginals, originalForContent, originalForDigest, prepareDocument, withUploadLock } from './uploads.mjs';

const uploadLimit = 1024 * 1024;
export const uploadInputLimit = 4 * Math.ceil(convertedLimit / 3);

export async function ingestSource(database, { sourceRoot, uploadRoot, parsers, keepOriginal, keepOriginals, sourceId: explicitId }, location, signal) {
  let id;
  let prepared;
  if (/^https?:\/\//iu.test(location)) {
    const url = new URL(location);
    if (url.username || url.password) throw new Error('Source URL must not contain credentials');
    const objectId = url.href;
    id = explicitId ?? sourceId(objectId);
    const { readHttpDocument } = await import('../../import/import-documents.mjs');
    const decoded = await readHttpDocument(url, signal,
      (body, type, name) => decodeRemote(body, type, name, { parsers, keepOriginal: keepOriginal ?? keepOriginals?.[id] === true, signal }));
    prepared = await prepareDocument(uploadRoot, id, { objectId, url: location }, decoded);
  } else {
    if (/^[a-z][a-z0-9+.-]*:\/\//iu.test(location) && !/^file:\/\//iu.test(location)) {
      throw new Error('Expected an HTTP(S) URL or a local file');
    }
    const path = await realpath(/^file:\/\//iu.test(location) ? fileURLToPath(location) : resolve(sourceRoot, location));
    const within = relative(sourceRoot, path);
    if (isAbsolute(within) || within === '..' || within.startsWith(`..${sep}`)) {
      throw new Error('File is outside the configured source root');
    }
    id = explicitId ?? sourceId(pathToFileURL(path).href);
    prepared = await prepareLocalFile(uploadRoot, id, path, basename(path), parsers, keepOriginal ?? keepOriginals?.[id] === true, signal);
  }
  return indexPrepared(database, id, prepared, signal);
}

export async function uploadSource(database, { uploadRoot, parsers }, input, signal) {
  const { name, content, encoding, contentType, originalUrl } = input;
  if (!name.trim() || name === '.' || name === '..' || /[/\\\x00-\x1f\x7f]/u.test(name)) {
    throw new Error('Expected a file name without directory components');
  }
  if (originalUrl !== undefined) validateOriginalUrl(originalUrl);
  const limit = parserCategory(parsers, name, contentType) && encoding === 'base64' ? convertedLimit : uploadLimit;
  const bytes = decodeUpload(content, encoding, limit);
  signal?.throwIfAborted();
  const decoded = await parseConfigured(parsers, bytes, name, contentType, signal)
    ?? await (await import('../../import/import-documents.mjs')).readDocument([bytes], contentType, name);
  const uploadId = input.uploadId ?? createHash('sha256')
    .update(JSON.stringify([name, originalUrl ?? null, decoded.format])).update(bytes).digest('hex');
  const id = `upload-${createHash('sha256').update(JSON.stringify([uploadId, name])).digest('hex').slice(0, 57)}`;
  const document = { sourceId: id, objectId: name };
  return withUploadLock(uploadRoot, document, async () => {
    const previous = input.uploadId && originalUrl === undefined ? await database.getDocument(document, { signal }) : null;
    const url = originalUrl ?? previous?.evidence.source.url ?? `dunlin://${id}/${encodeURIComponent(name)}`;
    signal?.throwIfAborted();
    const prepared = await prepareDocument(uploadRoot, id, { objectId: name, url }, { ...decoded, bytes });
    return { uploadId, ...await indexPrepared(database, id, prepared, signal) };
  });
}

async function indexPrepared(database, sourceId, { input, original }, signal) {
  signal?.throwIfAborted();
  await database.syncDocuments(sourceId, [input], { signal });
  return { ...(original && { original }), ...await describeSource(database, { sourceId, objectId: input.objectId }, signal) };
}

export async function readSource(database, uploadRoot, { sourceId, objectId, version, startLine, maxLines }, signal) {
  const ref = { sourceId, objectId };
  const archived = version && await database.getCorpusDocument(ref, version, { signal });
  const result = archived || await database.getDocument(ref, { signal });
  if (!result) throw new Error('Document not found');
  if (version !== undefined && result.evidence.source.version !== version) throw new Error('Source version has changed. Read the current source before continuing.');
  const totalLines = result.evidence.range.endLine;
  if (!totalLines) return { document: result.evidence.source, content: '', evidence: null, totalLines, nextLine: null };
  const endLine = Math.min(totalLines, startLine + maxLines - 1);
  if (startLine > endLine) throw new Error('Start line is outside the document');
  const corpus = archived || await database.getCorpusDocument(ref, result.evidence.source.version, { signal });
  const request = { ...result.evidence, range: { startLine, endLine } };
  const read = corpus ? readCorpusLines(corpus, request) : await database.read(request, { signal });
  const originals = await listOriginals(uploadRoot, read.evidence.source);
  const evidence = corpus ? corpusEvidence(read.evidence, corpus) : await enrichEvidence(uploadRoot, read.evidence, result.content);
  return {
    document: read.evidence.source, ...read, totalLines, nextLine: endLine < totalLines ? endLine + 1 : null,
    evidence,
    ...(originals.length ? { originals } : {}),
  };
}

export async function validateCitations(database, uploadRoot, citations, signal) {
  const verified = [];
  for (const citation of citations) {
    const { evidence } = await database.read(citation, { signal }).catch(async (error) => {
      if (error.code !== 'STALE_EVIDENCE') throw error;
      // A different original can produce the same parsed text and locations.
      const old = await database.getCorpusDocument(citation.source, citation.source.version, { signal });
      const current = await database.getDocument(citation.source, { signal });
      if (!old || !current || current.evidence.source.url !== citation.source.url) throw error;
      const latest = await database.getCorpusDocument(citation.source, current.evidence.source.version, { signal });
      if (old.parsedVersion !== latest?.parsedVersion) throw error;
      readCorpusLines(old, citation);
      return { evidence: { ...old.evidence, range: citation.range } };
    });
    const { source } = evidence;
    const corpus = await database.getCorpusDocument(source, source.version, { signal });
    if (corpus) {
      if (citation.original && citation.original.sha256 !== corpus.originalSha256) throw new Error('Original file does not match the cited source');
      verified.push(corpusEvidence(evidence, corpus));
    } else if (citation.original || (await listOriginals(uploadRoot, source)).length) {
      const document = await database.getDocument(source, { signal });
      if (!document || document.evidence.source.version !== source.version || document.evidence.source.url !== source.url) {
        throw new Error('Source changed while verifying its original file');
      }
      verified.push(await enrichEvidence(uploadRoot, evidence, document.content, citation.original?.sha256));
    } else {
      verified.push(evidence);
    }
  }
  return verified;
}

export function readCorpusLines(corpus, evidence) {
  const { startLine, endLine } = evidence.range;
  if (corpus.evidence.source.url !== evidence.source.url || startLine < 1
    || endLine < startLine || endLine > corpus.evidence.range.endLine) throw new Error('Evidence does not match the corpus revision');
  const lines = corpus.content.match(/[^\n]*\n|[^\n]+$/gu) ?? [];
  return { evidence, content: lines.slice(startLine - 1, endLine).join('') };
}

function corpusEvidence(evidence, corpus) {
  const locations = corpus.locations?.filter(({ range }) => range.startLine <= evidence.range.endLine && evidence.range.startLine <= range.endLine);
  const original = corpus.originalSha256 && originalForDigest(evidence.source, corpus.originalSha256);
  return { ...evidence, ...(original && { original }), ...(locations?.length && { locations }) };
}

async function enrichEvidence(root, evidence, content, requestedDigest) {
  const original = await originalForContent(root, evidence.source, content, requestedDigest);
  if (!original) return evidence;
  const locations = await citedLocations(root, evidence.source, content, original, evidence.range);
  return { ...evidence, original, ...(locations.length ? { locations } : {}) };
}

function sourceId(location) {
  return `src-${createHash('sha256').update(location).digest('hex').slice(0, 60)}`;
}

function decodeUpload(content, encoding, limit) {
  if (encoding === 'base64') {
    const bytes = Buffer.from(content, 'base64');
    if (bytes.toString('base64') !== content) throw new Error('Expected standard Base64 file content');
    if (bytes.length > limit) throw new Error(`Uploaded file exceeds ${limit / uploadLimit} MiB`);
    return bytes;
  }
  if (!content.isWellFormed()) throw new Error('Expected valid UTF-8 file content');
  if (Buffer.byteLength(content) > uploadLimit) throw new Error('Uploaded file exceeds 1 MiB');
  return Buffer.from(content, 'utf8');
}

function validateOriginalUrl(value) {
  const url = URL.parse(value);
  if (!url || !/^(?:https?|file|s3):\/\//iu.test(value) || /[\s\x00-\x1f\x7f]/u.test(value) || url.username || url.password) {
    throw new Error('Expected an HTTP(S), file, or S3 original URL without credentials');
  }
}

async function describeSource(database, document, signal) {
  const result = await database.getDocument(document, { signal });
  if (!result) throw new Error('The ingested document is unavailable');
  return { document: result.evidence.source, totalLines: result.evidence.range.endLine };
}
