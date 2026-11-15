import { copyFile, mkdir, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { createHash } from 'node:crypto';
import { Dunlin } from 'dunlin';

import { resolveApiKey, resolveConfigValue } from './config.mjs';
import { revision } from './authoring/files.mjs';
import { createEmbedder } from './models.mjs';
import { embedBatch } from './vectors.mjs';
import { chunkCorpus } from './project-content.mjs';
import { hannoy, openIndexTool } from './project-index-tool.mjs';
import { trialCheckpoint } from './project-trials.mjs';

function error(code, message) {
  return Object.assign(new Error(message), { code, exitCode: code === 'VECTOR_REQUIRED' ? 3 : 2 });
}

export function vectorModel(config, name) {
  const model = Object.hasOwn(config.models ?? {}, name) && config.models[name];
  if (model?.kind !== 'embedding' || !model.model) throw error('VECTOR_MODEL_REQUIRED', 'Define an embedding model under models.');
  try {
    return createEmbedder(model.provider, resolveConfigValue(model.model, 'model'), resolveConfigValue(model.base_url, 'base_url'),
      resolveApiKey(model.api_key_env, 'model'), { maxInputBytes: model.max_input_bytes });
  } catch { throw error('INVALID_VECTOR_MODEL', 'Check the embedding provider, model, endpoint and environment references.'); }
}

export async function semanticChunks(input, { max_chars, sentence_chars, similarity }, embedder, cache, signal) {
  const passages = chunkCorpus(input, { size: sentence_chars, overlap: 0 });
  const output = [];
  for (const [group, [, documents]] of input.groups.entries()) {
    const chunks = [];
    for (const [i, { content, format }] of documents.entries()) {
      const parts = passages[group][i];
      const key = revision(JSON.stringify(['chunk.semantic-v1', embedder.identity, max_chars, sentence_chars, similarity, input.unicode, format, revision(content)]));
      const saved = cache.read(key);
      if (saved) { chunks.push(saved); continue; }
      const bytes = Buffer.from(content);
      const text = parts.map((part) => part.searchText ?? bytes.subarray(part.byteRange.start, part.byteRange.end).toString());
      const vectors = [];
      const nonempty = text.map((part, index) => ({ part, index })).filter(({ part }) => part.trim());
      for (let offset = 0; offset < nonempty.length; offset += 32) {
        const batch = nonempty.slice(offset, offset + 32);
        const response = await embedBatch(embedder, batch.map(({ part }) => part), signal);
        for (const [j, { index }] of batch.entries()) vectors[index] = response[j];
      }
      const dimension = vectors.find(Boolean)?.length;
      if (vectors.some((vector) => vector.length !== dimension)) throw error('VECTOR_DIMENSIONS', 'Embedding dimensions differ within the document.');
      const merged = [];
      let length = 0;
      for (const [j, part] of parts.entries()) {
        const width = [...bytes.subarray(part.byteRange.start, part.byteRange.end).toString()].length;
        const previous = merged.at(-1);
        const related = vectors[j] && vectors[j - 1]
          && vectors[j].reduce((sum, n, coordinate) => sum + n * vectors[j - 1][coordinate], 0) >= similarity;
        if (previous && length + width <= max_chars && related) {
          previous.range.endLine = part.range.endLine;
          previous.byteRange.end = part.byteRange.end;
          if (input.unicode === 'nfkc') previous.searchText += text[j];
          length += width;
        } else {
          merged.push({ ...part, range: { ...part.range }, byteRange: { ...part.byteRange } });
          length = width;
        }
      }
      cache.write(key, merged);
      chunks.push(merged);
    }
    output.push(chunks);
  }
  return output;
}

export async function buildVectorIndex(state, database, path, source, transformed, embedder, force, backend, signal) {
  const identity = revision(JSON.stringify(embedder.identity));
  const lookup = state.prepare('SELECT data FROM embeddings WHERE id = ?');
  const save = state.prepare(force ? 'INSERT OR REPLACE INTO embeddings VALUES (?, ?)' : 'INSERT OR IGNORE INTO embeddings VALUES (?, ?)');
  const entries = [];
  const pending = new Map();
  for (const [group, [sourceId, documents]] of source.groups.entries()) {
    for (const [i, document] of documents.entries()) {
      trialCheckpoint();
      const current = await database.getDocument({ sourceId, objectId: document.objectId });
      if (!current) continue;
      const bytes = Buffer.from(document.content);
      for (const chunk of transformed.chunks[group][i]) {
        const text = chunk.searchText ?? bytes.subarray(chunk.byteRange.start, chunk.byteRange.end).toString();
        if (!text.trim()) continue;
        const key = revision(JSON.stringify([identity, text]));
        if (!pending.has(key)) pending.set(key, { key, text });
        const item = { title: chunk.heading || document.objectId, evidence: { ...current.evidence, range: chunk.range,
          byteRange: chunk.byteRange }, key };
        entries.push(item);
      }
    }
  }
  const vectors = new Map();
  const missing = [];
  for (const item of pending.values()) {
    const cached = !force && lookup.get(item.key);
    if (cached) vectors.set(item.key, cached.data);
    else missing.push(item);
  }
  for (let i = 0; i < missing.length; i += 32) {
    const batch = missing.slice(i, i + 32);
    const values = await embedBatch(embedder, batch.map(({ text }) => text), signal);
    for (const [j, { key }] of batch.entries()) {
      const bytes = floatBytes(values[j]);
      save.run(key, bytes);
      vectors.set(key, bytes);
    }
  }
  const width = vectors.values().next().value?.length ?? 0;
  if ([...vectors.values()].some((value) => value.length !== width)) throw error('VECTOR_DIMENSIONS', 'Embedding dimensions differ. Choose a fixed version of the model and rebuild.');
  const bytes = Buffer.concat(entries.map(({ key }) => vectors.get(key)));
  const metadata = { identity, dimensions: width / 4, entries };
  const stats = await backend.build(path, metadata, bytes);
  await writeFile(join(path, 'vectors.json'), JSON.stringify(metadata));
  return { identity, backend: backend.name, count: entries.length, output: createHash('sha256').update(bytes).digest('hex'), ...stats };
}

function floatBytes(values) {
  const bytes = Buffer.alloc(values.length * 4);
  for (const [i, value] of values.entries()) bytes.writeFloatLE(value, i * 4);
  return bytes;
}

// Each open database belongs to one immutable index. Concurrent searches share its load.
const metadataByIndex = new WeakMap();

function vectorMetadata(database, directory) {
  let load = metadataByIndex.get(database);
  if (!load) {
    load = readFile(join(directory, 'vectors.json'), 'utf8').then(JSON.parse).catch((failure) => {
      metadataByIndex.delete(database);
      throw failure;
    });
    metadataByIndex.set(database, load);
  }
  return load;
}

export function releaseVectorMetadata(database) {
  metadataByIndex.delete(database);
}

export async function vectorSearch(directory, build, embedder, query, { database, max, sourceIds, signal, backend }) {
  if (!build.vector) throw error('VECTOR_REQUIRED', 'The selected build has no vector index. Run dunlin build with index.vector configured.');
  if (build.vector.identity !== revision(JSON.stringify(embedder.identity))) {
    throw error('VECTOR_MODEL_MISMATCH', 'The query embedding model differs from the model used for the selected build.');
  }
  if ((build.vector.backend ?? 'exact') !== backend.name) throw error('VECTOR_BACKEND_MISMATCH', 'The selected build uses another vector index backend.');
  const { dimensions, entries } = await vectorMetadata(database, directory);
  let positions;
  if (sourceIds) {
    const sources = new Set(sourceIds);
    positions = [];
    for (const [i, { evidence }] of entries.entries()) {
      if (sources.has(evidence.source.sourceId)) positions.push(i);
    }
  }
  const totalHits = positions?.length ?? entries.length;
  if (!totalHits) return { hits: [], totalHits, engine: backend.engine, method: 'vector' };
  const [needle] = await embedBatch(embedder, [query], signal);
  signal?.throwIfAborted();
  if (needle.length !== dimensions) {
    throw error('VECTOR_DIMENSIONS', 'Query and index dimensions differ. Rebuild using the current embedding model.');
  }
  const ranked = await backend.search(directory, { needle, entries, positions, max, signal });
  const hits = ranked.map(({ i, score }) => ({ title: entries[i].title, evidence: entries[i].evidence, score }));
  return { hits, totalHits, engine: backend.engine, method: 'vector' };
}

export function vectorBackend(name, paths, { settings, baseline, signal } = {}) {
  if (name === 'exact') return {
    name, engine: 'vector-exact', file: 'vectors.f32',
    async build(directory, { entries }, bytes) {
      await writeFile(join(directory, 'vectors.f32'), bytes);
      return { indexed: entries.length, removed: 0 };
    },
    search(directory, { needle, entries, positions, max, signal }) {
      return Dunlin.rankVectors(join(directory, 'vectors.f32'), { needle, positions, rows: entries.length, limit: max }, { signal });
    },
  };
  if (name !== 'hannoy') throw error('VECTOR_BACKEND_MISMATCH', 'Unknown vector index backend.');
  return {
    name, engine: 'hannoy-hnsw', file: join('hannoy', 'data.mdb'),
    async build(directory, metadata, bytes) {
      const { identity, dimensions, entries } = metadata;
      const location = join(directory, 'hannoy');
      if (!entries.length) return { indexed: 0, removed: 0 };
      await mkdir(location);
      const tool = await openIndexTool(paths, hannoy, signal);
      const old = await compatibleGraph(baseline, metadata);
      if (old) await copyFile(join(baseline, 'hannoy', 'data.mdb'), join(location, 'data.mdb'));
      const prior = new Map((old?.entries ?? []).map(({ slot, id }) => [slot, id]));
      const inserts = [];
      const occurrences = new Map();
      let next = old?.next_id ?? 0;
      for (const [row, entry] of entries.entries()) {
        const { sourceId, objectId } = entry.evidence.source;
        const group = JSON.stringify([sourceId, objectId, entry.key]);
        const occurrence = occurrences.get(group) ?? 0;
        occurrences.set(group, occurrence + 1);
        entry.slot = revision(JSON.stringify([group, occurrence]));
        const id = prior.get(entry.slot);
        if (id !== undefined) { entry.id = id; prior.delete(entry.slot); }
        else {
          entry.id = next++;
          inserts.push({ id: entry.id, row });
        }
      }
      metadata.next_id = next;
      const file = join(directory, 'vectors.f32');
      await writeFile(file, bytes);
      try {
        const { inserted, deleted } = await tool.build({ directory: location, vectors: file, dimensions, rows: entries.length,
          inserts, deletes: [...prior.values()], ef_construction: settings.ef_construction });
        if (inserted !== inserts.length || deleted !== prior.size) throw error('VECTOR_BACKEND_MISMATCH', 'The index tool reported an incomplete build.');
        return { indexed: inserted, removed: deleted, reused: entries.length - inserted, identity };
      } finally { await rm(file, { force: true }); }
    },
    async search(directory, { needle, entries, positions, max, signal }) {
      const tool = await openIndexTool(paths, hannoy, signal);
      const allowed = new Map();
      for (const i of positions ?? entries.keys()) allowed.set(entries[i].id, i);
      const { hits } = await tool.search({ directory: join(directory, 'hannoy'), query: needle, limit: max,
        ef_search: settings.ef_search, candidates: positions ? [...allowed.keys()] : null });
      if (!Array.isArray(hits) || hits.length > max || hits.some((pair) => !Array.isArray(pair) || !allowed.has(pair[0]) || !Number.isFinite(pair[1]))) {
        throw error('VECTOR_BACKEND_MISMATCH', 'The index tool returned invalid candidates.');
      }
      return hits.map(([id, distance]) => ({ i: allowed.get(id), score: 1 - distance }));
    },
  };
}

async function compatibleGraph(baseline, { dimensions, identity }) {
  if (!baseline) return null;
  const old = JSON.parse(await readFile(join(baseline, 'vectors.json'), 'utf8'));
  if (old.dimensions !== dimensions || old.identity !== identity || !Number.isSafeInteger(old.next_id)) return null;
  const data = await stat(join(baseline, 'hannoy', 'data.mdb')).catch((error) => { if (error.code !== 'ENOENT') throw error; });
  return data?.isFile() ? old : null;
}

export function fuseResults(results, k, max) {
  const hits = new Map();
  for (const result of results) for (const [i, hit] of result.hits.entries()) {
    const { source, byteRange, range } = hit.evidence;
    const key = JSON.stringify([source.sourceId, source.objectId, source.version, byteRange ?? range]);
    if (!hits.has(key)) hits.set(key, { ...hit, score: 0 });
    hits.get(key).score += 1 / (k + 1 + i);
  }
  return { hits: [...hits.values()].sort((a, b) => b.score - a.score).slice(0, max),
    totalHits: Math.max(hits.size, ...results.map(({ totalHits }) => totalHits)), engine: 'retrieval-rrf', method: 'rrf' };
}
