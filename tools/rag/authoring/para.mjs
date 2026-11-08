import { readdir } from 'node:fs/promises';
import { join } from 'node:path';

import {
  atomicWrite, categories, checkPageRevision, checkSourceRevision, directory, idParts,
  pageFile, prepareRoot, readText, revision, withoutSources, withWriteLock,
} from './files.mjs';
import { readPublication, renderPublication } from './html.mjs';
import { validateCitations } from './sources.mjs';

export async function preparePara(path) {
  const root = await prepareRoot(path);
  for (const category of categories) await directory(join(root, category), true);
  return root;
}

export async function paraContext(root, id) {
  if (id !== undefined) {
    checkId(id);
    const file = await findNote(root, id);
    return { note: file ? readNote(id, file) : null };
  }
  const groups = {};
  const notes = new Set();
  for (const category of categories) {
    const path = join(root, category);
    if (!await directory(path)) continue;
    const entries = await readdir(path, { withFileTypes: true });
    groups[category] = entries.filter((entry) => entry.isDirectory() && !entry.name.startsWith('.'))
      .map(({ name }) => name).sort();
    await listNotes(path, category, notes);
  }
  return { root, groups, notes: [...notes].sort() };
}

export function savePara(root, database, uploadRoot, { id, title, markdown, citations, expectedRevision }, signal) {
  checkId(id);
  return withWriteLock(root, async () => {
    const previous = await findNote(root, id);
    checkPageRevision(id, previous?.content ?? null, expectedRevision);
    const sourceRevision = await database.getIndexRevision({ signal });
    const verified = await validateCitations(database, uploadRoot, citations, signal);
    const metadata = { title, markdown, citations: verified };
    const content = renderPublication(metadata, metadata);
    const path = await pageFile(root, id, 'html', true);
    await checkSourceRevision(database, sourceRevision, signal);
    const current = await findNote(root, id);
    checkPageRevision(id, current?.content ?? null, expectedRevision);
    await atomicWrite(path, content);
    return { note: { id, path, ...metadata, revision: revision(content) } };
  });
}

function checkId(id) {
  const parts = idParts(id);
  if (!categories.includes(parts[0]) || parts.length < 2) throw new Error('Expected a PARA category followed by a note ID');
}

async function findNote(root, id) {
  for (const extension of ['html', 'md']) {
    const path = await pageFile(root, id, extension);
    const content = await readText(path);
    if (content !== null) return { path, content, extension };
  }
  return null;
}

function readNote(id, { path, content, extension }) {
  const details = { id, path, revision: revision(content) };
  if (extension === 'html') return { ...details, ...readPublication(content) };
  const header = /^<!-- dunlin:para ([\w-]+) -->\r?\n/u.exec(content);
  const metadata = header ? JSON.parse(Buffer.from(header[1], 'base64url').toString('utf8')) : { title: id.split('/').at(-1), citations: [] };
  return { ...details, ...metadata, markdown: withoutSources(content.slice(header?.[0].length ?? 0)) };
}

async function listNotes(path, prefix, notes) {
  for (const entry of await readdir(path, { withFileTypes: true })) {
    if (entry.name.startsWith('.') || entry.isSymbolicLink()) continue;
    if (entry.isDirectory() && prefix.split('/').length < 8) {
      await listNotes(join(path, entry.name), `${prefix}/${entry.name}`, notes);
    } else if (entry.isFile() && /\.(?:html|md)$/u.test(entry.name)) {
      const id = `${prefix}/${entry.name.slice(0, entry.name.lastIndexOf('.'))}`;
      try { checkId(id); } catch { continue; }
      notes.add(id);
    }
  }
}
