import { randomUUID } from 'node:crypto';
import { mkdir, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';

import {
  atomicWrite, checkSourceRevision, directory, idParts, pageFile, prepareRoot, readText,
  revision, withoutSources, withWriteLock,
} from './files.mjs';
import { link, readPublication, renderDocument, renderMarkdown, renderPublication } from './html.mjs';
import { validateCitations } from './sources.mjs';

export async function prepareWiki(path) {
  const root = await prepareRoot(path);
  await directory(join(root, 'revisions'), true);
  return root;
}

export async function getWiki(root, id) {
  const active = await loadWiki(root);
  if (id === undefined) return { ...locations(active), pages: active.pages.map(({ id: key, title }) => ({ id: key, title })) };
  idParts(id);
  const metadata = active.pages.find((page) => page.id === id);
  if (!metadata) return { page: null };
  const { path, content } = await readPage(active, id);
  return { page: { ...metadata, path, revision: pageRevision(metadata, content), markdown: pageMarkdown(active, content) } };
}

export async function affectedWiki(root, database, signal) {
  const { pages } = await loadWiki(root);
  const sourceRevision = await database.getIndexRevision({ signal });
  const references = new Map();
  const affected = [];
  for (const { id, title, citations } of pages) {
    const changes = new Map();
    for (const { source } of citations) {
      const key = JSON.stringify([source.sourceId, source.objectId]);
      if (!references.has(key)) {
        const document = await database.getDocument(source, { signal });
        references.set(key, document?.evidence.source ?? null);
      }
      const current = references.get(key);
      if (!current || current.version !== source.version || current.url !== source.url) {
        changes.set(key, { previous: source, current, reason: current ? 'updated' : 'deleted' });
      }
    }
    if (changes.size) affected.push({ id, title, changes: [...changes.values()] });
  }
  await checkSourceRevision(database, sourceRevision, signal);
  return { affected };
}

export function applyWiki(root, database, uploadRoot, { pages, summary }, signal) {
  return withWriteLock(root, async () => {
    const active = await loadWiki(root);
    const previous = new Map();
    for (const page of active.pages) previous.set(page.id.toLowerCase(), { page, ...await readPage(active, page.id) });
    const next = new Map(previous);
    const changed = new Set();
    const sourceRevision = await database.getIndexRevision({ signal });
    for (const { id, title, markdown, citations, expectedRevision } of pages) {
      idParts(id);
      const key = id.toLowerCase();
      const old = previous.get(key);
      if (changed.has(key) || (old && old.page.id !== id)) throw new Error(`Duplicate or conflicting page ID ${id}`);
      if ((old ? pageRevision(old.page, old.content) : null) !== expectedRevision) {
        throw new Error(`Revision conflict for ${id}. Read the latest page before saving.`);
      }
      const verified = await validateCitations(database, uploadRoot, citations, signal);
      const page = { id, title, citations: verified };
      next.set(key, { page, content: renderPublication({ ...page, markdown }, { markdown }) });
      changed.add(key);
    }

    const token = randomUUID();
    const path = join(root, 'revisions', token);
    const pageRoot = join(path, 'pages');
    await mkdir(path);
    try {
      await mkdir(pageRoot);
      for (const [key, entry] of next) {
        const { page, content } = entry;
        const output = active.extension === 'md' && !changed.has(key)
          ? migratePage(page, content) : content;
        await writeFile(await pageFile(pageRoot, page.id, 'html', true), output, { flag: 'wx' });
      }
      const manifest = [...next.values()].map(({ page }) => page);
      const index = manifest.map(({ id, title }) => {
        const href = id.split('/').map(encodeURIComponent).join('/');
        return `<li>${link(`pages/${href}.html`, title)}</li>`;
      });
      const logFile = active.path && join(active.path, `log.${active.extension}`);
      const log = await readText(logFile);
      const oldLog = log === null ? '' : active.extension === 'md'
        ? log.replace(/^# 更新履歴\r?\n+/u, '') : readPublication(log).markdown;
      const logEntry = `- ${new Date().toISOString()} ${summary} (${pages.map(({ id }) => id).join(', ')})`;
      const logMarkdown = `${oldLog.trimEnd()}\n\n${logEntry}\n`.trimStart();
      await writeFile(join(path, 'manifest.json'), `${JSON.stringify({ format: 'html', pages: manifest }, null, 2)}\n`, { flag: 'wx' });
      await writeFile(join(path, 'index.html'), renderDocument('Wiki', `<p>${link('log.html', '更新履歴')}</p><ul>${index.join('\n')}</ul>`), { flag: 'wx' });
      await writeFile(join(path, 'log.html'), renderDocument('更新履歴', renderMarkdown(logMarkdown), { metadata: { markdown: logMarkdown } }), { flag: 'wx' });

      for (const old of previous.values()) {
        if (await readText(old.path) !== old.content) throw new Error(`Revision conflict for ${old.page.id}. Read the latest page before saving.`);
      }
      const pointer = join(root, 'CURRENT');
      const current = await readText(pointer);
      if ((current?.trim() ?? null) !== active.token || await readText(logFile) !== log) {
        throw new Error('The Wiki changed during this update. Read the latest revision before saving.');
      }
      await checkSourceRevision(database, sourceRevision, signal);
      await atomicWrite(pointer, `${token}\n`);
      return { ...locations({ token, path, extension: 'html' }), pages: [...changed].map((key) => {
        const { page, content } = next.get(key);
        return { id: page.id, revision: pageRevision(page, content), path: join(pageRoot, `${page.id}.html`) };
      }) };
    } catch (error) {
      await rm(path, { recursive: true, force: true });
      throw error;
    }
  });
}

async function loadWiki(root) {
  const pointer = await readText(join(root, 'CURRENT'));
  if (pointer === null) return { token: null, path: null, pages: [] };
  const token = pointer.trim();
  if (!/^[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12}$/u.test(token)) throw new Error('Invalid Wiki revision pointer');
  const revisions = join(root, 'revisions');
  if (!await directory(revisions)) throw new Error('Wiki revisions directory is missing');
  const path = join(revisions, token);
  if (!await directory(path)) throw new Error('Current Wiki revision is missing');
  const manifest = JSON.parse(await readText(join(path, 'manifest.json')));
  if (Array.isArray(manifest)) return { token, path, pages: manifest, extension: 'md' };
  if (manifest?.format !== 'html' || !Array.isArray(manifest.pages)) throw new Error('Invalid Wiki manifest');
  return { token, path, pages: manifest.pages, extension: 'html' };
}

async function readPage(active, id) {
  const root = join(active.path, 'pages');
  if (!await directory(root)) throw new Error('Wiki pages directory is missing');
  const path = await pageFile(root, id, active.extension);
  const content = await readText(path);
  if (content === null) throw new Error(`Wiki page ${id} is missing`);
  return { path, content };
}

function pageMarkdown({ extension }, content) {
  return extension === 'md' ? withoutSources(content) : readPublication(content).markdown;
}

function migratePage(page, content) {
  const markdown = withoutSources(content);
  return renderPublication({ ...page, markdown }, { markdown });
}

function pageRevision(page, content) {
  return revision(JSON.stringify([page, content]));
}

function locations({ token, path, extension }) {
  return { revision: token, indexPath: path && join(path, `index.${extension}`), logPath: path && join(path, `log.${extension}`) };
}
