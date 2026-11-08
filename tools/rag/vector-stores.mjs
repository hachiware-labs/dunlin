import { createHash, randomUUID } from 'node:crypto';
import { readFile, rename, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';

import { chromaServer, qdrantServer } from './vector-servers.mjs';
import { sqliteStore } from './vector-sqlite.mjs';

export function createVectorStore(indexPath, embedder, backend = 'local', url, apiKey) {
  if ((backend === 'local' || backend === 'sqlite') && url !== undefined) {
    throw new Error('--vector-store-url requires Chroma or Qdrant');
  }
  if ((backend === 'local' || backend === 'sqlite') && apiKey !== undefined) {
    throw new Error('rag.vectorStore.apiKeyEnv requires Chroma or Qdrant');
  }
  switch (backend) {
    case 'local':
      return localStore(storePath(indexPath, embedder.identity, 'json'));
    case 'sqlite':
      return sqliteStore(storePath(indexPath, [...embedder.identity, backend], 'sqlite'));
    case 'chroma':
    case 'qdrant': {
      const server = backend === 'chroma' ? chromaServer(url, apiKey) : qdrantServer(url, apiKey);
      return serverStore(storePath(indexPath, [...embedder.identity, ...server.identity], 'json'), server);
    }
    default:
      throw new Error('Expected --vector-store local, sqlite, chroma, or qdrant');
  }
}

function localStore(path) {
  return {
    save: (snapshot, beforePublish) => writeManifest(path, { version: 1, ...snapshot }, beforePublish),
    async load(sourceIds) {
      const snapshot = await readManifest(path);
      const scope = sourceIds && new Set(sourceIds);
      const entries = scope ? snapshot.entries.filter(({ evidence }) => scope.has(evidence.source.sourceId)) : snapshot.entries;
      return { ...snapshot, entries, count: entries.length };
    },
    search(snapshot, queryVector) {
      return snapshot.entries.map(({ evidence, vector }) => {
        if (vector.length !== snapshot.dimensions) throw new Error('Invalid stored embedding dimensions');
        const score = vector.reduce((sum, value, index) => sum + value * queryVector[index], 0);
        if (!Number.isFinite(score)) throw new Error('Invalid stored embedding');
        return { evidence, score };
      }).sort((a, b) => b.score - a.score);
    },
  };
}

function serverStore(path, server) {
  return {
    async save({ revision, dimensions, entries }, beforePublish) {
      const name = `dunlin-${randomUUID()}`;
      let collection;
      try {
        if (entries.length) {
          collection = await server.create(name, dimensions);
          for (let offset = 0; offset < entries.length; offset += 64) {
            const points = entries.slice(offset, offset + 64).map(({ evidence, vector }, index) => ({
              id: offset + index, vector, sourceId: evidence.source.sourceId,
            }));
            await server.upload(collection, points);
          }
        }
        await writeManifest(path, {
          version: 1, revision, dimensions, collection: collection ?? null,
          entries: entries.map(({ evidence }) => evidence),
        }, beforePublish);
      } catch (error) {
        if (collection) await server.remove(name).catch(() => {});
        throw error;
      }
    },
    async load(sourceIds) {
      const snapshot = await readManifest(path);
      if (snapshot.entries.length && (typeof snapshot.collection !== 'string' || !snapshot.collection)) {
        throw new Error('Invalid vector collection. Run dunlin embed to rebuild it.');
      }
      const scope = sourceIds && new Set(sourceIds);
      const count = scope ? snapshot.entries.filter(({ source }) => scope.has(source.sourceId)).length : snapshot.entries.length;
      return { ...snapshot, count };
    },
    async search(snapshot, vector, { sourceIds, limit }) {
      const ids = await server.search(snapshot.collection, vector, sourceIds, Math.min(snapshot.count, limit * 4));
      if (!Array.isArray(ids)) throw new Error('Vector server returned an invalid result');
      const scope = sourceIds && new Set(sourceIds);
      return ids.map((id) => {
        const number = typeof id === 'number' || typeof id === 'string' ? Number(id) : NaN;
        const evidence = Number.isSafeInteger(number) && String(number) === String(id) ? snapshot.entries[number] : undefined;
        if (!evidence || (scope && !scope.has(evidence.source.sourceId))) {
          throw new Error('Vector server returned a point outside the stored evidence');
        }
        return { evidence };
      });
    },
  };
}

async function readManifest(path) {
  const snapshot = JSON.parse(await readFile(path, 'utf8'));
  if (!Array.isArray(snapshot?.entries)) throw new Error('Invalid vector index. Run dunlin embed to rebuild it.');
  return snapshot;
}

async function writeManifest(path, snapshot, beforePublish) {
  const temporary = `${path}.${randomUUID()}.tmp`;
  try {
    await writeFile(temporary, JSON.stringify(snapshot), { flag: 'wx' });
    await beforePublish();
    await rename(temporary, path);
  } finally {
    await rm(temporary, { force: true });
  }
}

function storePath(indexPath, identity, extension) {
  const key = createHash('sha256').update(JSON.stringify(identity)).digest('hex');
  return join(indexPath, `.dunlin-vectors-${key}.${extension}`);
}
