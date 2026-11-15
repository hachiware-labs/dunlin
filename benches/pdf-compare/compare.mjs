// Compares PDF text extraction by pdf_oxide (Rust, the default) and pdfjs (the previous reader)
// (spec 28.9 step 2a). Prints a Markdown table and, with --show, both texts of each PDF that differs.
//
// Usage: node benches/pdf-compare/compare.mjs <directory of PDFs> [--runs <n>] [--show]
//
// pdf_oxide runs through Dunlin.loadCorpus, as builds use it, with the cache bypassed.
import { mkdtemp, readdir, readFile, rm } from 'node:fs/promises';
import { DatabaseSync } from 'node:sqlite';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { parseArgs } from 'node:util';

import { Dunlin } from '../../dist/index.js';
import { pdfText } from '../../tools/rag/parsers/documents.mjs';

const { values, positionals } = parseArgs({ allowPositionals: true, options: { runs: { type: 'string', default: '3' }, show: { type: 'boolean' } } });
if (positionals.length !== 1) {
  console.error('Usage: node benches/pdf-compare/compare.mjs <directory of PDFs> [--runs <n>] [--show]');
  process.exit(2);
}
const directory = resolve(positionals[0]);
const runs = Math.max(1, Number(values.runs));
const names = (await readdir(directory)).filter((name) => name.toLowerCase().endsWith('.pdf')).sort();

const work = await mkdtemp(join(tmpdir(), 'dunlin-pdf-compare-'));
const state = join(work, 'state.sqlite');
const setup = new DatabaseSync(state);
setup.exec('CREATE TABLE loaded_files (id TEXT PRIMARY KEY, document TEXT NOT NULL)');
setup.close();

async function oxide(file) {
  const request = { state, originals: join(work, 'originals'), force: true, ocrVersion: '', root: work,
    files: [{ id: 'pdf', file, objectId: file, kind: 'pdf', route: 'loader.auto' }] };
  const { documents: [document] } = await Dunlin.loadCorpus(request, { parse: () => { throw new Error('unexpected host call'); } });
  return { content: document.content, locations: document.corpus.locations ?? [] };
}

async function timed(read) {
  const times = [];
  let result;
  for (let run = 0; run < runs; run++) {
    const start = performance.now();
    try {
      result = await read();
    } catch (error) {
      result = { error: error.message };
    }
    times.push(performance.now() - start);
  }
  times.sort((a, b) => a - b);
  return { ...result, ms: times[Math.floor(times.length / 2)] };
}

const compact = (text) => [...(text ?? '').replace(/\s+/gu, '')];

/** 1 - edit distance / longer length, over characters without whitespace. */
function similarity(a, b) {
  const [x, y] = [compact(a), compact(b)];
  if (!x.length && !y.length) return 1;
  let previous = Array.from({ length: y.length + 1 }, (_, index) => index);
  for (let i = 1; i <= x.length; i++) {
    const current = [i];
    for (let j = 1; j <= y.length; j++) {
      current[j] = Math.min(previous[j] + 1, current[j - 1] + 1, previous[j - 1] + (x[i - 1] === y[j - 1] ? 0 : 1));
    }
    previous = current;
  }
  return 1 - previous[y.length] / Math.max(x.length, y.length);
}

const pages = ({ locations }) => (locations ?? []).map(({ page }) => page).join(',');
const rows = [];
try {
  for (const name of names) {
    const file = join(directory, name);
    const bytes = await readFile(file);
    const pdfjs = await timed(() => pdfText(bytes));
    const rust = await timed(() => oxide(file));
    rows.push({ name, pdfjs, rust });
  }
} finally {
  await rm(work, { recursive: true, force: true, maxRetries: 5 });
}

const cell = (value) => String(value).replaceAll('|', '\\|');
console.log('| PDF | Same text | Same without whitespace | Similarity | Same pages | pdfjs ms | pdf_oxide ms |');
console.log('| --- | --- | --- | --- | --- | --- | --- |');
for (const { name, pdfjs, rust } of rows) {
  const failed = pdfjs.error || rust.error;
  console.log(`| ${cell(name)} | ${failed ? '-' : pdfjs.content === rust.content ? 'yes' : 'no'} | ${failed ? '-' : compact(pdfjs.content).join('') === compact(rust.content).join('') ? 'yes' : 'no'} | `
    + `${failed ? '-' : similarity(pdfjs.content, rust.content).toFixed(3)} | ${failed ? '-' : pages(pdfjs) === pages(rust) ? 'yes' : `no (${pages(pdfjs)} / ${pages(rust)})`} | `
    + `${pdfjs.error ? 'error' : pdfjs.ms.toFixed(1)} | ${rust.error ? 'error' : rust.ms.toFixed(1)} |`);
}
const differing = rows.filter(({ pdfjs, rust }) => pdfjs.error || rust.error || pdfjs.content !== rust.content);
console.log(`\n${differing.length} of ${rows.length} PDFs differ.`);
if (values.show) {
  for (const { name, pdfjs, rust } of differing) {
    console.log(`\n### ${name}\n\npdfjs:\n\n\`\`\`text\n${pdfjs.error ?? pdfjs.content}\n\`\`\`\n\npdf_oxide:\n\n\`\`\`text\n${rust.error ?? rust.content}\n\`\`\``);
  }
}
