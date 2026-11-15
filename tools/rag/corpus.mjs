import { join, resolve } from 'node:path';

import { Dunlin } from 'dunlin';
import { resolveCorpusBackend } from './config.mjs';

import { atomicWrite, directory, prepareRoot, readText, withWriteLock } from './authoring/files.mjs';
import { inspectVectorIndex } from './vectors.mjs';

const corpusDirectory = '.dunlin-corpus-v1';
const manifestFile = 'manifest.json';

export async function describeCorpus(index, config, getVectorStore) {
  const info = await readCorpusInfo(index);
  let snapshot;
  let indexError;
  try {
    snapshot = await Dunlin.inspectIndex(index);
  } catch (error) {
    indexError = error.message;
  }
  const configured = new Set(Object.keys(config?.config.sources ?? {}));
  const counts = new Map(Object.entries(snapshot?.sources ?? {}));
  const sourceIds = [...new Set([...configured, ...counts.keys()])].sort();
  const sources = sourceIds.map((sourceId) => ({
    sourceId,
    configured: configured.has(sourceId),
    documents: indexError ? null : counts.get(sourceId) ?? 0,
  }));
  const searchableDocuments = snapshot?.searchableDocuments ?? 0;
  const keyword = indexError
    ? { state: 'unavailable', error: indexError }
    : { state: !snapshot ? 'not_indexed' : searchableDocuments > 0 ? 'ready' : 'empty', searchableDocuments };

  let vector = { state: 'not_configured' };
  if (getVectorStore) {
    if (keyword.state === 'ready') {
      try {
        vector = await inspectVectorIndex(getVectorStore(), snapshot.revision);
      } catch (error) {
        vector = { state: 'unavailable', error: error.message };
      }
    } else {
      vector = { state: 'needs_keyword_index' };
    }
  }
  return {
    ...info,
    status: { configPath: config?.path ?? null, indexPath: resolve(index), corpusBackend: resolveCorpusBackend(config?.config), sources, search: { keyword, vector } },
  };
}

export async function readCorpusInfo(index) {
  const root = join(index, corpusDirectory);
  if (!await directory(root)) return { version: 1 };
  const path = join(root, manifestFile);
  const text = await readText(path);
  if (text === null) return { version: 1 };
  try {
    const info = JSON.parse(text.replace(/^\uFEFF/u, ''));
    if (info?.version !== 1) throw new Error('Expected corpus manifest version 1');
    if (info.name !== undefined) validateText('name', info.name);
    if (info.description !== undefined) validateText('description', info.description);
    if (info.metadata !== undefined && (!info.metadata || typeof info.metadata !== 'object' || Array.isArray(info.metadata))) {
      throw new Error('Expected metadata to be an object');
    }
    return info;
  } catch (error) {
    throw new Error(`Invalid corpus manifest at ${path}: ${error.message}`, { cause: error });
  }
}

export async function setCorpusInfo(index, key, input) {
  const metadataKey = /^metadata\.(\S(?:[^\r\n]*\S)?)$/u.exec(key)?.[1];
  let value = input;
  if (key === 'name' || key === 'description') {
    validateText(key, input);
  } else if (metadataKey) {
    try {
      value = JSON.parse(input);
    } catch {
      // An unquoted value is stored as text.
    }
  } else {
    throw new Error('Expected name, description, or metadata.<key>');
  }

  const indexRoot = await prepareRoot(index);
  const root = join(indexRoot, corpusDirectory);
  await directory(root, true);
  const path = join(root, manifestFile);
  await withWriteLock(root, async () => {
    const current = await readCorpusInfo(indexRoot);
    const updated = metadataKey
      ? { ...current, metadata: { ...current.metadata, [metadataKey]: value } }
      : { ...current, [key]: value };
    await atomicWrite(path, `${JSON.stringify(updated, null, 2)}\n`);
  });
  return path;
}

function validateText(key, value) {
  if (typeof value !== 'string' || !value.trim() || value.includes('\0') || (key === 'name' && /[\r\n]/u.test(value))) {
    throw new Error(`Expected ${key} to be nonempty ${key === 'name' ? 'single-line ' : ''}text`);
  }
}
