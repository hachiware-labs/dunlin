import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { Dunlin } from 'dunlin';

import { resolveApiKey, resolveConfigValue } from './config.mjs';
import { revision } from './authoring/files.mjs';
import { embedderFor, embeddingEndpoint } from './models.mjs';
import { embedBatch } from './vectors.mjs';
import { hannoy, openIndexTool } from './project-index-tool.mjs';

function error(code, message) {
  return Object.assign(new Error(message), { code, exitCode: code === 'VECTOR_REQUIRED' ? 3 : 2 });
}

/** The embedding model `name` under `models`, resolved for requests: `{ provider, model, url, apiKey, maxInputBytes }`. */
export function embeddingSettings(config, name) {
  const model = Object.hasOwn(config.models ?? {}, name) && config.models[name];
  if (model?.kind !== 'embedding' || !model.model) throw error('VECTOR_MODEL_REQUIRED', 'Define an embedding model under models.');
  try {
    return { ...embeddingEndpoint(model.provider, resolveConfigValue(model.model, 'model'), resolveConfigValue(model.base_url, 'base_url'),
      resolveApiKey(model.api_key_env, 'model')), ...(model.max_input_bytes !== undefined && { maxInputBytes: model.max_input_bytes }) };
  } catch { throw error('INVALID_VECTOR_MODEL', 'Check the embedding provider, model, endpoint and environment references.'); }
}

export function vectorModel(config, name) {
  return embedderFor(embeddingSettings(config, name));
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

/** The search side of a vector index backend. The index stage builds them in Rust (index_stage.rs). */
export function vectorBackend(name, paths, { settings, signal } = {}) {
  if (name === 'exact') return {
    name, engine: 'vector-exact', file: 'vectors.f32',
    search(directory, { needle, entries, positions, max, signal }) {
      return Dunlin.rankVectors(join(directory, 'vectors.f32'), { needle, positions, rows: entries.length, limit: max }, { signal });
    },
  };
  if (name !== 'hannoy') throw error('VECTOR_BACKEND_MISMATCH', 'Unknown vector index backend.');
  return {
    name, engine: 'hannoy-hnsw', file: join('hannoy', 'data.mdb'),
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
