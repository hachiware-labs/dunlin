
import { extname, join, matchesGlob, relative, sep } from 'node:path';
import { open, realpath, stat } from 'node:fs/promises';
import { createHash } from 'node:crypto';

import { isPlainDocument, listFiles, prepareLocalFile } from './ingest-files.mjs';
import { lightonModel } from './parsers/catalog.mjs';
import { managedOcrVersion } from './parsers/lighton-setup.mjs';
import { trialCheckpoint, trialSignal } from './project-trials.mjs';

const office = new Set(['.docx', '.xlsx', '.pptx']);
const formats = new Set(['.html', '.htm', '.pdf', ...office]);
const images = new Set(['.png', '.jpg', '.jpeg', '.webp', '.bmp', '.gif']);
const extensionsFor = { text: new Set(['.html', '.htm']), office, pdf: new Set(['.pdf']) };

function supported(file, categories) {
  const extension = extname(file).toLowerCase();
  return categories.some((category) => category === 'auto' ? isPlainDocument(file) || formats.has(extension)
    : category === 'ocr' ? extension === '.pdf' || images.has(extension)
    : category === 'text' && isPlainDocument(file) || extensionsFor[category]?.has(extension));
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

export function unsupportedFiles(sources, ocr = false, categories = ['auto']) {
  const skipped = [];
  for (const { id, files } of sources) {
    for (const { file, objectId } of files) {
      if (!supported(file, categories) && !(ocr && supported(file, ['ocr']))) {
        skipped.push({ source_id: id, path: objectId, reason: 'No configured loader for this file format' });
      }
    }
  }
  return skipped;
}

export async function inventory(sources, previous) {
  const old = new Map((previous?.groups ?? []).flatMap(([id, docs]) => docs.map(({ objectId, corpus }) =>
    [JSON.stringify([id, objectId]), corpus?.originalSha256])));
  const files = [];
  for (const { id, files: paths } of sources) for (const { file, objectId } of paths) {
    const key = JSON.stringify([id, objectId]);
    const handle = await open(file, 'r');
    let actual;
    try {
      const hash = createHash('sha256');
      for await (const chunk of handle.createReadStream({ autoClose: false })) hash.update(chunk);
      actual = hash.digest('hex');
    } finally { await handle.close(); }
    const change = !old.has(key) ? 'added' : old.get(key) === actual ? 'unchanged' : 'updated';
    old.delete(key);
    files.push({ source_id: id, path: objectId, change });
  }
  for (const key of old.keys()) {
    const [source_id, path] = JSON.parse(key);
    files.push({ source_id, path, change: 'deleted' });
  }
  return files;
}

export async function loadCorpus(sources, paths, skipped, warnings, db, force, ocr = false, progress = () => {}) {
  const excluded = new Set(skipped.map(({ source_id, path }) => JSON.stringify([source_id, path])));
  const groups = [];
  const saved = db.prepare('SELECT document FROM loaded_files WHERE id = ?');
  const save = db.prepare('INSERT OR IGNORE INTO loaded_files VALUES (?, ?)');
  let completed = 0;
  progress(completed);
  for (const { id, files } of sources) {
    const documents = [];
    for (const { file, objectId } of files) {
      if (excluded.has(JSON.stringify([id, objectId]))) continue;
      trialCheckpoint();
      try {
        const extension = extname(file).toLowerCase();
        const parsers = office.has(extension) ? { documents: { tool: 'docling-rs' } } : undefined;
        const useOcr = ocr && (extension === '.pdf' || images.has(extension));
        const key = (hash) => JSON.stringify(['loader.auto-v1', id, objectId, extension, hash, ...(useOcr ? [managedOcrVersion] : [])]);
        const cache = {
          read: (hash) => {
            const row = !force && saved.get(key(hash));
            return row && JSON.parse(row.document);
          },
          write: (hash, doc) => save.run(key(hash), JSON.stringify(doc)),
        };
        const { input } = await prepareLocalFile(join(paths.artifacts, 'originals'), id, file, objectId, parsers, true,
          trialSignal(), useOcr ? loadOcr : extension === '.pdf' ? loadPdf : ['.html', '.htm'].includes(extension) ? loadHtml : undefined, cache,
          { sharedOriginals: true });
        if (!input.content.trim()) {
          warnings.push({ code: 'EMPTY_EXTRACTION', section: 'source', module: 'loader.auto', target: id + ':' + objectId,
            message: 'No text extracted. Scanned documents require OCR.' });
        }
        documents.push(input);
        progress(++completed);
      } catch (error) {
        if (error.code === 'TRIAL_LIMIT_REACHED') throw error;
        throw new Error('Could not load ' + id + ':' + objectId + '. ' + error.message, { cause: error });
      }
    }
    groups.push([id, documents]);
  }
  return groups;
}

async function loadOcr(bytes) {
  const { parseOcr } = await import('./parsers/ocr.mjs');
  const parsed = await parseOcr(bytes, { tool: 'lightonocr', managed: true, model: lightonModel, lang: 'ja' }, trialSignal());
  return { ...parsed, bytes, parser: { tool: 'lightonocr', model: lightonModel, lang: 'ja' } };
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

const graphemes = new Intl.Segmenter('und', { granularity: 'grapheme' });

export function chunkCorpus({ groups, unicode }, { size, overlap, structure = false }, cache) {
  return groups.map(([, documents]) => documents.map(({ content, format, corpus }) => {
    trialCheckpoint();
    const boundaries = structure && new Set((corpus?.locations ?? []).filter(({ page }) => page).map(({ range }) => range.startLine));
    const sections = structure && new Set(content.split('\n').flatMap((text, i) =>
      /^\s{0,3}#{1,6}\s+\S/u.test(text) ? [i + 1] : []));
    const key = cache && JSON.stringify([structure ? 'chunk.structure-v1' : 'chunk.length-v1', unicode, size, overlap, format,
      cache.hash(content), ...(structure ? [[...boundaries]] : [])]);
    const saved = key && cache.read(key);
    if (saved) return saved;
    const units = [];
    let byte = 0;
    let line = 1;
    for (const { segment, index } of graphemes.segment(content)) {
      if (units.length % 1024 === 0) trialCheckpoint();
      units.push({ index, byte, line, count: [...segment].length, breakAfter: /[\n。.!?]$/u.test(segment) });
      byte += Buffer.byteLength(segment);
      if (segment.endsWith('\n')) line++;
    }
    if (!units.length) return [];
    units.push({ index: content.length, byte, line });
    const chunks = [];
    const last = units.length - 1;
    for (let start = 0, end = 0; end < last;) {
      const previous = end;
      let length = 0;
      end = start;
      while (end < last && (end === start || length + units[end].count <= size)) {
        const line = units[end].line;
        if (structure && end > start && line !== units[end - 1].line && (boundaries.has(line) || sections.has(line))) break;
        length += units[end++].count;
      }
      while (end <= previous && end < last) end++;
      const min = Math.max(start, previous, end - Math.floor((end - start) / 4));
      if (!structure && end < last) {
        for (let candidate = end - 1; candidate > min; candidate--) {
          if (units[candidate - 1].breakAfter) { end = candidate; break; }
        }
      }
      const first = units[start];
      const next = units[end];
      const text = content.slice(first.index, next.index);
      const heading = format === 'markdown' ? text.match(/^\s{0,3}#{1,6} +([^\r\n]+)/mu)?.[1]?.trim() ?? '' : '';
      chunks.push({
        heading,
        range: { startLine: first.line, endLine: units[end - 1].line },
        byteRange: { start: first.byte, end: next.byte },
        ...(unicode === 'nfkc' && { searchText: text.normalize('NFKC') }),
      });
      let tail = 0;
      let nextStart = end;
      while (nextStart > start + 1 && tail + units[nextStart - 1].count <= overlap) tail += units[--nextStart].count;
      start = nextStart;
    }
    if (key) cache.write(key, chunks);
    return chunks;
  }));
}
