import assert from 'node:assert/strict';
import { once } from 'node:events';
import { mkdtemp, rm } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { test } from 'node:test';
import { join } from 'node:path';

import { Dunlin } from 'dunlin';

import { answerQuestion } from '../rag.mjs';
import { createVectorStore } from '../vector-stores.mjs';
import { buildVectors, searchVectors } from '../vectors.mjs';

const fact = 'Backup snapshots expire after 37 days.';
const url = 'https://example.invalid/backups';
const question = 'Preservation lifespan?';
const vectorKey = 'fixture-vector-key';
const embedder = {
  identity: ['fixture'],
  embed: async (input) => input.map((text) => text === question || text.includes(fact) ? [1, 0] : [0, 1]),
};

test('SQLite, Chroma and Qdrant store vectors separately and preserve Dunlin citations', { timeout: 30_000 }, async (t) => {
  const collections = new Map();
  let failUpload = false;
  const server = createServer(async (request, response) => {
    const { pathname } = new URL(request.url, 'http://localhost');
    const parts = pathname.split('/');
    const chroma = pathname.startsWith('/api/v2/');
    const collectionIndex = parts.indexOf('collections') + 1;
    const name = parts[collectionIndex];
    const action = parts.at(-1);
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const body = chunks.length ? JSON.parse(Buffer.concat(chunks).toString('utf8')) : {};
    const send = (status, value) => response.writeHead(status, { 'content-type': 'application/json' }).end(JSON.stringify(value));
    if (request.headers[chroma ? 'x-chroma-token' : 'api-key'] !== vectorKey) return send(401, { error: 'Missing API key' });
    if (request.method === 'DELETE') {
      collections.delete(name);
      return send(200, true);
    }
    if (chroma ? action === 'collections' : request.method === 'PUT' && action === name) {
      const collection = chroma ? body.name : name;
      const metric = chroma ? body.configuration.hnsw.space : body.vectors.distance;
      if (metric.toLowerCase() !== 'cosine') return send(400, { error: 'Wrong distance metric' });
      collections.set(collection, []);
      return send(200, chroma ? { id: collection } : { result: true });
    }
    if (!collections.has(name)) return send(404, { error: 'Missing collection' });
    if (action === 'upsert' || action === 'points') {
      if (failUpload) return send(503, { error: 'Unavailable' });
      const points = chroma ? body.ids.map((id, index) => ({
        id, vector: body.embeddings[index], payload: body.metadatas[index],
      })) : body.points;
      if (points.some(({ payload }) => Object.keys(payload).join() !== 'sourceId')) return send(400, { error: 'Unexpected metadata' });
      collections.get(name).push(...points);
      return send(200, { result: { status: 'completed' } });
    }
    const scope = chroma ? body.where?.sourceId.$in : body.filter?.must[0].match.any;
    const vector = chroma ? body.query_embeddings[0] : body.query;
    const ids = collections.get(name).filter(({ payload }) => !scope || scope.includes(payload.sourceId))
      .sort((a, b) => b.vector[0] * vector[0] - a.vector[0] * vector[0])
      .slice(0, body.n_results ?? body.limit).map(({ id }) => id);
    return send(200, chroma ? { ids: [ids] } : { result: { points: ids.map((id) => ({ id })) } });
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  t.after(() => new Promise((resolve) => server.close(resolve)));
  const origin = `http://127.0.0.1:${server.address().port}`;
  const root = await mkdtemp(join(tmpdir(), 'dunlin-vector-stores-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const database = await Dunlin.open(root);
  try {
    await database.replaceDocuments('gardening', [{ objectId: 'garden', url: 'https://example.invalid/garden', format: 'text', content: 'Prune roses in spring.' }]);
    await database.replaceDocuments('policy', [{ objectId: 'backup', url, format: 'markdown', content: `# Policy\n${fact}\n` }]);
    const backends = ['sqlite', 'chroma', 'qdrant'];
    for (const backend of backends) {
      const store = backend === 'sqlite'
        ? createVectorStore(root, embedder, backend)
        : createVectorStore(root, embedder, backend, origin, vectorKey);
      const retrieve = (query, options) => searchVectors(database, embedder, store, query, options);
      assert.deepEqual(await buildVectors(database, embedder, store), { documents: 2, chunks: 2, dimensions: 2 });
      if (backend === 'sqlite') {
        const snapshot = await database.snapshotDocuments();
        let checks = 0;
        const changingIndex = {
          snapshotDocuments: async () => snapshot,
          getIndexRevision: async () => ++checks === 1 ? snapshot.revision : 'changed',
        };
        await assert.rejects(buildVectors(changingIndex, embedder, store), /Vector index is stale/u);
      } else {
        const count = collections.size;
        failUpload = true;
        await assert.rejects(buildVectors(database, embedder, store), /HTTP 503/u);
        failUpload = false;
        assert.equal(collections.size, count);
      }
      const answer = await answerQuestion(database, question, { retrieve }, async (_instructions, input) => {
        const { evidence: [first] } = JSON.parse(input);
        assert.equal(first.lines[1].text, fact);
        return JSON.stringify({ claims: [{ text: fact, citations: [{ id: first.id, startLine: 2, endLine: 2 }] }] });
      });
      assert.deepEqual(answer.citations, [{ id: 1, url, range: { startLine: 2, endLine: 2 }, content: `${fact}\n` }]);
      const scoped = await retrieve(question, { sourceIds: ['gardening'], limit: 8 });
      assert.deepEqual(scoped.map(({ evidence }) => evidence.source.sourceId), ['gardening']);
    }
    await database.removeSource('policy');
    for (const backend of backends) {
      const store = backend === 'sqlite'
        ? createVectorStore(root, embedder, backend)
        : createVectorStore(root, embedder, backend, origin, vectorKey);
      await assert.rejects(searchVectors(database, embedder, store, question, { limit: 8 }), /Vector index is stale/u);
      await buildVectors(database, embedder, store);
      assert.deepEqual(await searchVectors(database, embedder, store, question, { sourceIds: ['policy'], limit: 8 }), []);
    }
  } finally {
    await database.close();
  }
});
