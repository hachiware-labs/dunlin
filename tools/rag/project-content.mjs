
import { join, matchesGlob, relative, sep } from 'node:path';
import { open, realpath, stat } from 'node:fs/promises';
import { createHash } from 'node:crypto';

import { Dunlin } from 'dunlin';

import { decodeRemote, listFiles } from './ingest-files.mjs';
import { lightonModel } from './parsers/catalog.mjs';
import { managedOcrVersion } from './parsers/lighton-setup.mjs';
import { commandIdentity, commandVersion, defaultRoutes, parserKind, parseWithCommand, resolveParser } from './project-parsers.mjs';
import { trialCheckpoint, trialSignal } from './project-trials.mjs';

const fileConcurrency = 4;

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
      pending.push({ id, file, objectId, documents, route, kind: parserKind(route, file) });
    }
  }
  // Resolve each command parser's version once, before any file is parsed with it.
  const versions = new Map();
  for (const route of new Set(pending.filter(({ kind }) => kind === 'command').map(({ route }) => route))) {
    versions.set(route, await commandVersion(route, root, trialSignal()));
  }
  // Rust reads, hashes, caches and stores. It calls back here for the parsers that are still in JavaScript.
  const byFile = new Map(pending.map((item) => [JSON.stringify([item.id, item.objectId]), item]));
  const host = {
    progress,
    async parse({ id, objectId, kind, name }, bytes) {
      const { route } = byFile.get(JSON.stringify([id, objectId]));
      const signal = trialSignal();
      const parsed = kind === 'command' ? await parseWithCommand(route, bytes, name, { root, version: versions.get(route), signal })
        : kind === 'ocr' ? await loadOcr(bytes)
        : kind === 'pdf' ? await loadPdf(bytes)
        : kind === 'html' ? await loadHtml(bytes)
        : await decodeRemote([bytes], undefined, name, { parsers: { documents: { tool: 'docling-rs' } }, keepOriginal: true, signal });
      return { content: parsed.content, format: parsed.format, locations: parsed.locations, parser: parsed.parser };
    },
  };
  const request = {
    state: paths.state, originals: join(paths.artifacts, 'originals', 'shared'), force: Boolean(force), ocrVersion: managedOcrVersion,
    files: pending.map(({ id, file, objectId, kind, route }) => ({ id, file, objectId, kind, route: route.name,
      ...(kind === 'command' && { command: commandIdentity(route, versions.get(route)) }) })),
  };
  progress(0);
  let loaded;
  try {
    loaded = await Dunlin.loadCorpus(request, host, { signal: trialSignal() });
  } catch (error) {
    trialCheckpoint();
    if (error.code === 'LOAD_FAILED') throw new Error(error.message, { cause: error });
    throw error;
  }
  loaded.documents.forEach((document, index) => pending[index].documents.push(document));
  warnings.push(...loaded.warnings);
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
