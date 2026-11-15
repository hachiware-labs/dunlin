const chunkBytes = 2048;
const chunkLines = 16;
const batchSize = 16;

export async function buildVectors(database, embedder, store) {
  const { revision, documents } = await database.snapshotDocuments();
  const entries = [];
  let dimensions = 0;
  let batch = [];

  async function saveBatch() {
    const vectors = await embedBatch(embedder, batch.map(({ content }) => content));
    dimensions ||= vectors[0].length;
    for (const [index, vector] of vectors.entries()) {
      if (vector.length !== dimensions) throw new Error('Embedding dimensions changed during indexing');
      entries.push({ evidence: batch[index].evidence, vector });
    }
    batch = [];
  }

  for (const document of documents) {
    for (const chunk of documentChunks(document)) {
      batch.push(chunk);
      if (batch.length === batchSize) await saveBatch();
    }
  }
  if (batch.length) await saveBatch();

  await requireRevision(database, revision);
  await store.save({ revision, dimensions, entries }, () => requireRevision(database, revision));
  return { documents: documents.length, chunks: entries.length, dimensions };
}

export async function searchVectors(database, embedder, store, query, { sourceIds, limit }) {
  if (sourceIds && (sourceIds.length > 100 || sourceIds.some((id) => !/^[a-zA-Z0-9_.-]{1,64}$/u.test(id)))) {
    throw new Error('Expected up to 100 valid source IDs');
  }
  const snapshot = await loadSnapshot(store, sourceIds);
  if (!snapshot) throw new Error('Vector index missing. Run dunlin embed with the same vector store and embedding settings.');
  await requireRevision(database, snapshot.revision);
  if (!snapshot.count) return [];

  const [queryVector] = await embedBatch(embedder, [query]);
  if (queryVector.length !== snapshot.dimensions) throw new Error('Embedding dimensions changed. Run dunlin embed to rebuild the index.');
  const ranked = await store.search(snapshot, queryVector, { sourceIds, limit });
  await requireRevision(database, snapshot.revision);

  const hits = [];
  const seen = new Set();
  for (const hit of ranked) {
    const { source, range } = hit.evidence;
    const key = JSON.stringify([source.sourceId, source.objectId, range.startLine, range.endLine]);
    if (seen.has(key)) continue;
    seen.add(key);
    hits.push(hit);
    if (hits.length === limit) break;
  }
  return hits;
}

export async function inspectVectorIndex(store, revision) {
  const snapshot = await loadSnapshot(store);
  if (!snapshot) return { state: 'missing' };
  const state = snapshot.revision !== revision ? 'stale' : snapshot.count > 0 ? 'ready' : 'empty';
  return { state, chunks: snapshot.count };
}

async function loadSnapshot(store, sourceIds) {
  const snapshot = await store.load(sourceIds).catch((error) => {
    if (error.code !== 'ENOENT') throw error;
  });
  if (!snapshot) return null;
  if (snapshot.version !== 1 || typeof snapshot.revision !== 'string'
    || !Number.isSafeInteger(snapshot.dimensions) || snapshot.dimensions < 0
    || !Number.isSafeInteger(snapshot.count) || snapshot.count < 0) {
    throw new Error('Invalid vector index. Run dunlin embed to rebuild it.');
  }
  return snapshot;
}

export async function embedBatch(embedder, input, signal) {
  const vectors = await embedder.embed(input, signal);
  if (!Array.isArray(vectors) || vectors.length !== input.length) {
    throw new Error('Embedding provider returned the wrong number of vectors');
  }
  return Array.from(vectors, (vector) => {
    if (!Array.isArray(vector) || !vector.length || vector.some((value) => !Number.isFinite(value))) {
      throw new Error('Embedding provider returned an invalid vector');
    }
    const norm = Math.sqrt(vector.reduce((sum, value) => sum + value * value, 0));
    if (!Number.isFinite(norm) || norm === 0) throw new Error('Embedding provider returned an invalid vector norm');
    return vector.map((value) => value / norm);
  });
}

async function requireRevision(database, revision) {
  if (await database.getIndexRevision() !== revision) throw new Error('Vector index is stale. Run dunlin embed to rebuild it.');
}

function* documentChunks({ evidence, content }) {
  const lines = content.match(/[^\n]*\n|[^\n]+$/gu) ?? [];
  let startLine = 1;
  let buffer = '';
  let bytes = 0;
  const makeChunk = (text, start, end) => ({ content: text, evidence: { ...evidence, range: { startLine: start, endLine: end } } });

  for (const [index, line] of lines.entries()) {
    const number = index + 1;
    const size = Buffer.byteLength(line);
    if (bytes + size > chunkBytes || number - startLine >= chunkLines) {
      if (buffer.trim()) yield makeChunk(buffer, startLine, number - 1);
      buffer = '';
      bytes = 0;
      startLine = number;
    }
    if (size > chunkBytes) {
      for (const part of splitLine(line)) {
        if (part.trim()) yield makeChunk(part, number, number);
      }
      startLine = number + 1;
      continue;
    }
    buffer += line;
    bytes += size;
  }
  if (buffer.trim()) yield makeChunk(buffer, startLine, lines.length);
}

function* splitLine(line) {
  let buffer = '';
  let bytes = 0;
  for (const char of line) {
    const size = Buffer.byteLength(char);
    if (bytes + size > chunkBytes) {
      yield buffer;
      buffer = '';
      bytes = 0;
    }
    buffer += char;
    bytes += size;
  }
  if (buffer) yield buffer;
}
