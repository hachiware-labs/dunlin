
import { join } from 'node:path';

import { KnowledgeNest } from 'knowledge-nest';

import { convertOffice } from './parsers/documents.mjs';
import { lightonModel } from './parsers/catalog.mjs';
import { lightonServer, managedOcr, managedOcrVersion } from './parsers/lighton-setup.mjs';
import { oarOcrHome } from './parsers/oar-setup.mjs';
import { defaultRoutes, routeFiles } from './project-parsers.mjs';

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
    state: paths.state, corpus: paths.corpus, originals: join(paths.artifacts, 'originals', 'shared'), force: Boolean(force), ...await ocrRequest(ocr),
    root: root ?? process.cwd(),
    files: pending.map(({ id, file, objectId, kind, route }) => ({ id, file, objectId, kind, route: route.name,
      ...(kind === 'command' && { command: { module: route.name, args: route.args } }) })),
  };
  progress(0);
  let loaded;
  try {
    loaded = await KnowledgeNest.loadCorpus(request, host);
  } catch (error) {
    if (error.code === 'LOAD_FAILED') throw new Error(error.message, { cause: error });
    throw error;
  }
  loaded.documents.forEach((document, index) => pending[index].documents.push(document));
  warnings.push(...loaded.warnings);
  return groups;
}

/** Rust reads, hashes, caches, runs loader.command and stores. It calls back here for the parsers that are still in JavaScript. */
export function parserHost(progress = () => {}) {
  return {
    progress,
    async parse({ kind, name }, bytes) {
      const parsed = kind === 'html' ? await loadHtml(bytes) : await convertOffice(bytes, name);
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

// Chunk documents in native batches of about this many characters.
const chunkBatchChars = 4 * 1024 * 1024;

export function chunkCorpus({ groups, unicode }, { size, overlap, structure = false }, cache) {
  const output = groups.map(([, documents]) => new Array(documents.length));
  const pending = [];
  groups.forEach(([, documents], group) => documents.forEach(({ content, format, corpus }, index) => {
    const boundaries = structure ? [...new Set((corpus?.locations ?? []).filter(({ page }) => page).map(({ range }) => range.startLine))] : [];
    const key = cache && JSON.stringify([structure ? 'chunk.structure-v1' : 'chunk.length-v1', unicode, size, overlap, format,
      cache.hash(content), ...(structure ? [boundaries] : [])]);
    const saved = key && cache.read(key);
    if (saved) output[group][index] = saved;
    else pending.push({ group, index, key, document: { content, markdown: format === 'markdown', ...(boundaries.length && { boundaries }) } });
  }));
  const options = { size, overlap, structure, nfkc: unicode === 'nfkc' };
  for (let start = 0; start < pending.length;) {
    let end = start;
    for (let chars = 0; end < pending.length && (end === start || chars < chunkBatchChars); end++) chars += pending[end].document.content.length;
    const batch = pending.slice(start, end);
    const chunks = KnowledgeNest.chunkDocuments(batch.map(({ document }) => document), options);
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
