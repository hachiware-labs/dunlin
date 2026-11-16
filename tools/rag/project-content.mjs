
import { join } from 'node:path';
import { open } from 'node:fs/promises';
import { createHash } from 'node:crypto';

import { Dunlin } from 'dunlin';

import { convertOffice } from './parsers/documents.mjs';
import { lightonModel } from './parsers/catalog.mjs';
import { lightonServer, managedOcr, managedOcrVersion } from './parsers/lighton-setup.mjs';
import { oarOcrHome } from './parsers/oar-setup.mjs';
import { defaultRoutes, routeFiles } from './project-parsers.mjs';
import { chargeTrialApi, trialCheckpoint, trialSignal } from './project-trials.mjs';

const fileConcurrency = 4;

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

/** Scans a folder in Rust. Files are ordered here, by locale, until identifiers change (spec 28.9). */
export async function scanFolder(sources, { path, exclude }, { name, projectRoot }) {
  const { id, files } = await Dunlin.scanSource({ path, exclude, name, ...(projectRoot && { projectRoot }) }, { signal: trialSignal() });
  files.sort((a, b) => a.objectId.localeCompare(b.objectId));
  return [...sources, { id, files }];
}

/** Routes every scanned file once: `{ route, kind }` or null, keyed by source and object ID. */
function routeSources(sources, routes) {
  const all = sources.flatMap(({ id, files }) => files.map((file) => ({ id, ...file })));
  const routed = routeFiles(routes, all);
  return new Map(all.map(({ id, objectId }, index) => [JSON.stringify([id, objectId]), routed[index]]));
}

/** Returns the files that no parser claims. routes come from parserRoutes or defaultRoutes. */
export function unsupportedFiles(sources, routes = defaultRoutes()) {
  const routed = routeSources(sources, routes);
  return sources.flatMap(({ id, files }) => files.filter(({ objectId }) => !routed.get(JSON.stringify([id, objectId])))
    .map(({ objectId }) => ({ source_id: id, path: objectId, reason: 'No configured loader for this file format' })));
}

/** Names the parser module for each scanned file, or null when none claims it. */
export function parserAssignments(sources, routes) {
  return new Map([...routeSources(sources, routes)].map(([key, routed]) => [key, routed?.route.name ?? null]));
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

/**
 * The OCR fields of a load request. lightonocr keeps its former cache key part, so its results stay
 * valid; oar-ocr adds its name and version.
 */
export async function ocrRequest(ocr) {
  if (ocr?.tool === 'oar-ocr') return { ocrVersion: 'oar-ocr@' + ocr.version, ocrTool: 'oar-ocr', ocrHome: oarOcrHome() };
  // Rust starts the managed server when a page needs OCR. Without it, those files fail with a hint.
  const managed = ocr ? await managedOcr({ required: false }) : undefined;
  return { ocrVersion: managedOcrVersion, ocrTool: 'lightonocr',
    ...(managed && { lighton: { python: managed.python, server: lightonServer, modelDir: managed.modelDir, model: lightonModel } }) };
}

export async function loadCorpus(sources, paths, skipped, warnings, db, force, routes = defaultRoutes(), progress = () => {}, root, ocr = null) {
  const excluded = new Set(skipped.map(({ source_id, path }) => JSON.stringify([source_id, path])));
  const groups = [];
  const pending = [];
  const routed = routeSources(sources, routes);
  for (const { id, files } of sources) {
    const documents = [];
    groups.push([id, documents]);
    for (const { file, objectId } of files) {
      const key = JSON.stringify([id, objectId]);
      if (excluded.has(key)) continue;
      const { route, kind } = routed.get(key);
      pending.push({ id, file, objectId, documents, route, kind });
    }
  }
  const host = parserHost(progress);
  const request = {
    state: paths.state, originals: join(paths.artifacts, 'originals', 'shared'), force: Boolean(force), ...await ocrRequest(ocr),
    root: root ?? process.cwd(),
    files: pending.map(({ id, file, objectId, kind, route }) => ({ id, file, objectId, kind, route: route.name,
      ...(kind === 'command' && { command: { module: route.name, args: route.args } }) })),
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

/**
 * Runs the `source` stage in Rust: routing, loading, the stage ID and its record in state.sqlite.
 * Returns `{ output, reused, warnings }`, or `{ unsupported: { files, skipped } }`.
 */
export async function runSourceStage({ sources, routes, reject, skipUnsupported, force, modules, ocr = null }, paths, root) {
  const request = {
    state: paths.state, originals: join(paths.artifacts, 'originals', 'shared'), root, force: Boolean(force),
    skipUnsupported: Boolean(skipUnsupported), reject: Boolean(reject), ...await ocrRequest(ocr), sources,
    routes: routes.map(({ name, category, match, ocr }) => ({ name, category, ...(match && { match }), ocr })),
    commands: routes.filter(({ category }) => category === 'command').map(({ name, args }) => ({ module: name, args })),
    modules,
  };
  let result;
  try {
    result = await Dunlin.runSource(request, parserHost(), { signal: trialSignal() });
  } catch (error) {
    trialCheckpoint();
    if (error.code === 'LOAD_FAILED') throw new Error(error.message, { cause: error });
    throw error;
  }
  return result.status === 'unsupported' ? { unsupported: result.stats }
    : { output: result.output, reused: result.reused, warnings: result.warnings };
}

/**
 * Runs the transform stage in Rust (spec 28.9 step 2b): chunking, the chunk cache, embedding calls for
 * chunk.semantic and the stage record. Model requests count against trial limits here.
 * Returns `{ output, reused }`.
 */
export async function runTransformStage({ sourceId, modules, force, embedder = null }, paths) {
  let refused;
  const host = {
    charge(bytes) {
      try {
        chargeTrialApi({ byteLength: bytes });
      } catch (error) {
        refused = error;
        throw error;
      }
    },
  };
  let result;
  try {
    result = await Dunlin.runTransform({ state: paths.state, sourceId, force: Boolean(force), modules, embedder }, host, { signal: trialSignal() });
  } catch (error) {
    trialCheckpoint();
    throw error;
  }
  if (result.status === 'failed') {
    if (refused) throw refused;
    const { code, message, exitCode } = result.error;
    throw Object.assign(new Error(message), code && { code, exitCode });
  }
  return { output: result.output, reused: result.reused };
}

/** Rust reads, hashes, caches, runs loader.command and stores. It calls back here for the parsers that are still in JavaScript. */
export function parserHost(progress = () => {}) {
  return {
    progress,
    async parse({ kind, name }, bytes) {
      const signal = trialSignal();
      const parsed = kind === 'html' ? await loadHtml(bytes)
        : await convertOffice(bytes, name, signal);
      return { content: parsed.content, format: parsed.format, locations: parsed.locations, parser: parsed.parser };
    },
  };
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
