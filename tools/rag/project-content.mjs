
import { basename, extname, join, matchesGlob, relative, sep } from 'node:path';
import { open, realpath, stat } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { pathToFileURL } from 'node:url';

import { Dunlin } from 'dunlin';

import { revision } from './authoring/files.mjs';
import { decodeRemote, listFiles } from './ingest-files.mjs';
import { lightonModel } from './parsers/catalog.mjs';
import { managedOcrVersion } from './parsers/lighton-setup.mjs';
import { commandIdentity, commandVersion, defaultRoutes, parserKind, parseWithCommand, resolveParser } from './project-parsers.mjs';
import { trialCheckpoint, trialSignal } from './project-trials.mjs';

const fileConcurrency = 4;
// Each commit costs a disk sync, about 1 ms on Windows. Group cache rows into short transactions.
const flushRows = 256;
const flushMs = 1000;

/** Runs synchronous writes in one savepoint, which also works inside an open transaction. */
export function writeTogether(db, run) {
  db.exec('SAVEPOINT dunlin_cache');
  try {
    run();
    db.exec('RELEASE dunlin_cache');
  } catch (error) {
    db.exec('ROLLBACK TO dunlin_cache');
    db.exec('RELEASE dunlin_cache');
    throw error;
  }
}

async function processFiles(files, prepare, record, limit = fileConcurrency) {
  const results = new Array(files.length);
  let next = 0;
  let failure;
  await Promise.all(Array.from({ length: Math.min(files.length, limit) }, async () => {
    while (!failure && next < files.length) {
      const index = next++;
      try {
        results[index] = await prepare(files[index]);
      } catch (error) {
        if (!failure || index < failure.index) failure = { index, error };
      }
    }
  }));
  // Drain in-flight work before the caller can close the database. Record results in input order.
  for (let index = 0; index < (failure?.index ?? files.length); index++) record(files[index], results[index]);
  if (failure) throw failure.error;
}

export async function scanFolder(sources, { path, exclude }, { name, projectRoot }) {
  const root = await realpath(path);
  if (!(await stat(root)).isDirectory()) throw new Error('Expected a directory for ' + name);
  const id = name === 'source.filesystem' ? 'docs' : name;
  const files = [];
  for await (const file of listFiles(root)) {
    trialCheckpoint();
    const objectId = relative(root, file).split(sep).join('/');
    if (projectRoot && file === join(projectRoot, 'Dunlinfile.yaml')) continue;
    if (!exclude.some((pattern) => matchesGlob(objectId, pattern))) files.push({ file, objectId });
  }
  files.sort((a, b) => a.objectId.localeCompare(b.objectId));
  return [...sources, { id, files }];
}

/** Returns the files that no parser claims. routes come from parserRoutes or defaultRoutes. */
export function unsupportedFiles(sources, routes = defaultRoutes()) {
  const skipped = [];
  for (const { id, files } of sources) {
    for (const { file, objectId } of files) {
      if (!resolveParser(routes, file, objectId)) {
        skipped.push({ source_id: id, path: objectId, reason: 'No configured loader for this file format' });
      }
    }
  }
  return skipped;
}

/** Names the parser module for each scanned file, or null when none claims it. */
export function parserAssignments(sources, routes) {
  return new Map(sources.flatMap(({ id, files }) => files.map(({ file, objectId }) =>
    [JSON.stringify([id, objectId]), resolveParser(routes, file, objectId)?.name ?? null])));
}

export async function inventory(sources, previous) {
  const old = new Map((previous?.groups ?? []).flatMap(([id, docs]) => docs.map(({ objectId, corpus }) =>
    [JSON.stringify([id, objectId]), corpus?.originalSha256])));
  const files = [];
  const pending = sources.flatMap(({ id, files }) => files.map((entry) => ({ ...entry, id })));
  await processFiles(pending, async ({ file }) => {
    const handle = await open(file, 'r');
    try {
      const hash = createHash('sha256');
      for await (const chunk of handle.createReadStream({ autoClose: false })) hash.update(chunk);
      return hash.digest('hex');
    } finally { await handle.close(); }
  }, ({ id, objectId }, actual) => {
    const key = JSON.stringify([id, objectId]);
    const change = !old.has(key) ? 'added' : old.get(key) === actual ? 'unchanged' : 'updated';
    old.delete(key);
    files.push({ source_id: id, path: objectId, change });
  });
  for (const key of old.keys()) {
    const [source_id, path] = JSON.parse(key);
    files.push({ source_id, path, change: 'deleted' });
  }
  return files;
}

// Native reads cover up to this many files and bytes at a time; originals are stored in batches.
const readFiles = 256;
const readBytes = 64 * 1024 * 1024;
const textLimit = 1024 * 1024;
const convertedLimit = 16 * textLimit;

export async function loadCorpus(sources, paths, skipped, warnings, db, force, routes = defaultRoutes(), progress = () => {}, root) {
  const excluded = new Set(skipped.map(({ source_id, path }) => JSON.stringify([source_id, path])));
  const groups = [];
  const pending = [];
  for (const { id, files } of sources) {
    const documents = [];
    groups.push([id, documents]);
    for (const { file, objectId } of files) {
      if (excluded.has(JSON.stringify([id, objectId]))) continue;
      const route = resolveParser(routes, file, objectId);
      pending.push({ id, file, objectId, documents, route, kind: parserKind(route, file), index: pending.length });
    }
  }
  const ocr = pending.some(({ kind }) => kind === 'ocr');
  // Resolve each command parser's version once, before any file is parsed with it.
  const versions = new Map();
  for (const route of new Set(pending.filter(({ kind }) => kind === 'command').map(({ route }) => route))) {
    versions.set(route, await commandVersion(route, root, trialSignal()));
  }
  const saved = db.prepare('SELECT document FROM loaded_files WHERE id = ?');
  const save = db.prepare('INSERT OR IGNORE INTO loaded_files VALUES (?, ?)');
  const sharedOriginals = join(paths.artifacts, 'originals', 'shared');

  // Rust reads files ahead in batches. Workers take files in order, so reads advance monotonically.
  const limitFor = (kind) => kind === 'text' ? textLimit : convertedLimit;
  const ready = new Map();
  let readFrom = 0;
  let reading;
  async function read(index) {
    while (!ready.has(index)) {
      reading ??= (async () => {
        const batch = pending.slice(readFrom, readFrom + readFiles).map(({ file, kind }) => ({ path: file, limit: limitFor(kind) }));
        const results = await Dunlin.readSourceFiles(batch, readBytes, { signal: trialSignal() });
        results.forEach((result, offset) => ready.set(readFrom + offset, result));
        readFrom += results.length;
      })().finally(() => { reading = undefined; });
      await reading;
    }
    const result = ready.get(index);
    ready.delete(index);
    if (result.error === 'too_large') throw new Error(`Source file exceeds ${limitFor(pending[index].kind) / textLimit} MiB`);
    if (result.error === 'changed') throw new Error('A source file changed while it was being indexed');
    if (result.error === 'not_file') throw new Error('Expected a local file');
    if (result.error) throw new Error(result.message ?? 'Could not read the source file');
    return result;
  }

  // Originals are stored before the cache rows that refer to them. Parsing can wait on OCR or
  // external commands, so both are buffered and written between files.
  let originals = new Map();
  let originalBytes = 0;
  let rows = [];
  let flushedAt = Date.now();
  let flushing = Promise.resolve();
  function flush() {
    const storing = [...originals];
    const writing = rows;
    originals = new Map();
    originalBytes = 0;
    rows = [];
    flushedAt = Date.now();
    flushing = flushing.then(async () => {
      if (storing.length) {
        const errors = await Dunlin.storeOriginals(sharedOriginals, storing.map(([sha256, { bytes }]) => ({ sha256, bytes })));
        const failed = errors.findIndex((error) => error);
        if (failed >= 0) throw Object.assign(new Error('Could not load ' + storing[failed][1].target + '. ' + errors[failed]), { loadTarget: true });
      }
      if (writing.length) writeTogether(db, () => { for (const [key, document] of writing) save.run(key, document); });
    });
    return flushing;
  }

  let completed = 0;
  progress(completed);
  // Managed OCR serves one request at a time. Keep its document and trial accounting order.
  const limit = ocr ? 1 : fileConcurrency;
  let failed = false;
  try {
    await processFiles(pending, async ({ index, id, file, objectId, route, kind }) => {
      trialCheckpoint();
      try {
        const extension = extname(file).toLowerCase();
        const useOcr = kind === 'ocr';
        const version = versions.get(route);
        const key = kind === 'command'
          ? (hash) => JSON.stringify(['loader.command-v1', id, objectId, hash, commandIdentity(route, version)])
          : (hash) => JSON.stringify(['loader.auto-v1', id, objectId, extension, hash, ...(useOcr ? [managedOcrVersion] : [])]);
        const { bytes, modifiedAtMs } = await read(index);
        const document = { objectId, url: pathToFileURL(file).href, modifiedAtMs: modifiedAtMs ?? undefined };
        const hash = revision(bytes);
        const row = !force && saved.get(key(hash));
        if (row) {
          progress(++completed);
          return { ...JSON.parse(row.document), ...document };
        }
        const name = basename(file);
        const signal = trialSignal();
        let parsed;
        if (kind === 'command') parsed = await parseWithCommand(route, bytes, name, { root, version, signal });
        else if (useOcr) parsed = await loadOcr(bytes);
        else if (kind === 'pdf') parsed = await loadPdf(bytes);
        else if (kind === 'html') parsed = await loadHtml(bytes);
        else if (kind === 'office') {
          parsed = await decodeRemote([bytes], undefined, name, { parsers: { documents: { tool: 'docling-rs' } }, keepOriginal: true, signal });
        } else {
          const content = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(bytes);
          parsed = { content, format: ['.md', '.markdown'].includes(extension) ? 'markdown' : 'text', bytes };
        }
        if (Buffer.byteLength(parsed.content) > textLimit) throw new Error('Extracted text exceeds 1 MiB');
        signal?.throwIfAborted();
        const { content, format, locations, parser } = parsed;
        const originalSha256 = parsed.bytes && revision(parsed.bytes);
        if (originalSha256 && !originals.has(originalSha256)) {
          originals.set(originalSha256, { bytes: parsed.bytes, target: id + ':' + objectId });
          originalBytes += parsed.bytes.length;
        }
        const input = { ...document, content, format, corpus: { locations, parser, originalSha256 } };
        rows.push([key(hash), JSON.stringify(input)]);
        if (rows.length >= flushRows || originalBytes >= readBytes || Date.now() - flushedAt >= flushMs) await flush();
        progress(++completed);
        return input;
      } catch (error) {
        if (error.code === 'TRIAL_LIMIT_REACHED' || error.loadTarget) throw error;
        throw Object.assign(new Error('Could not load ' + id + ':' + objectId + '. ' + error.message, { cause: error }), { loadTarget: true });
      }
    }, ({ id, objectId, documents, route }, input) => {
      if (!input.content.trim()) {
        warnings.push({ code: 'EMPTY_EXTRACTION', section: 'source', module: route.name, target: id + ':' + objectId,
          message: 'No text extracted. Scanned documents require OCR.' });
      }
      documents.push(input);
    }, limit);
  } catch (error) {
    failed = true;
    throw error;
  } finally {
    // Keep the files that finished, so a failed or cancelled build can reuse them.
    try {
      await flush();
    } catch (error) {
      if (!failed) throw error;
    }
  }
  return groups;
}

async function loadOcr(bytes) {
  const { parseOcr } = await import('./parsers/ocr.mjs');
  try {
    const parsed = await parseOcr(bytes, { tool: 'lightonocr', managed: true, model: lightonModel, lang: 'ja' }, trialSignal());
    return { ...parsed, bytes, parser: { tool: 'lightonocr', model: lightonModel, lang: 'ja' } };
  } catch (error) {
    if (error.code === 'OCR_NOT_PREPARED') throw new Error('Local GPU OCR is not prepared. Run dunlin add ocr.', { cause: error });
    throw error;
  }
}

async function loadPdf(bytes) {
  const { pdfText } = await import('./parsers/documents.mjs');
  const { content, locations } = await pdfText(bytes, trialSignal());
  return { content, locations, format: 'text', bytes, parser: { tool: 'pdfjs' } };
}

async function loadHtml(bytes) {
  const { load } = await import('cheerio');
  const $ = load(new TextDecoder('utf-8', { fatal: true }).decode(bytes));
  $('script, style, noscript, template, nav, footer, aside, [hidden]').remove();
  for (const heading of $('h1, h2, h3, h4, h5, h6').toArray()) {
    $(heading).prepend('#'.repeat(Number(heading.tagName.slice(1))) + ' ');
  }
  const root = $('main, [role="main"]').first();
  const article = $('article').first();
  const body = root.length ? root : article.length ? article : $('body');
  body.find('h1,h2,h3,h4,h5,h6,p,li,blockquote,pre,div,section,tr,br').before('\n').after('\n');
  const content = body.text().split(/\r?\n/u).map((line) => line.trim()).filter(Boolean).join('\n');
  return { content, format: 'markdown', bytes, parser: { tool: 'cheerio' } };
}

// Chunk documents in native batches of about this many characters, checking trial limits between them.
const chunkBatchChars = 4 * 1024 * 1024;

export function chunkCorpus({ groups, unicode }, { size, overlap, structure = false }, cache) {
  const output = groups.map(([, documents]) => new Array(documents.length));
  const pending = [];
  groups.forEach(([, documents], group) => documents.forEach(({ content, format, corpus }, index) => {
    trialCheckpoint();
    const boundaries = structure ? [...new Set((corpus?.locations ?? []).filter(({ page }) => page).map(({ range }) => range.startLine))] : [];
    const key = cache && JSON.stringify([structure ? 'chunk.structure-v1' : 'chunk.length-v1', unicode, size, overlap, format,
      cache.hash(content), ...(structure ? [boundaries] : [])]);
    const saved = key && cache.read(key);
    if (saved) output[group][index] = saved;
    else pending.push({ group, index, key, document: { content, markdown: format === 'markdown', ...(boundaries.length && { boundaries }) } });
  }));
  const options = { size, overlap, structure, nfkc: unicode === 'nfkc' };
  for (let start = 0; start < pending.length;) {
    trialCheckpoint();
    let end = start;
    for (let chars = 0; end < pending.length && (end === start || chars < chunkBatchChars); end++) chars += pending[end].document.content.length;
    const batch = pending.slice(start, end);
    const chunks = Dunlin.chunkDocuments(batch.map(({ document }) => document), options);
    const store = () => batch.forEach(({ group, index, key }, position) => {
      output[group][index] = chunks[position];
      if (key) cache.write(key, chunks[position]);
    });
    if (cache?.together) cache.together(store);
    else store();
    start = end;
  }
  return output;
}
