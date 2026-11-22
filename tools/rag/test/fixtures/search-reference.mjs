// The JavaScript search before it moved to Rust (spec 28.10 step 3a): retrieval, fusion, reranking,
// context assembly, search_config_id and the returned format. Kept to check that the Rust version
// gives the same results. Copied from project.mjs, project-vectors.mjs, vectors.mjs and
// project-search-output.mjs.
import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';

import { KnowledgeNest } from 'knowledge-nest';
import { z } from 'zod';

import { resolveApiKey, resolveConfigValue } from './cli-reference/config.mjs';
import { createGenerator, embedderFor, embeddingEndpoint } from './cli-reference/models.mjs';
import { hannoy, openIndexTool } from './cli-reference/project-index-tool.mjs';
import { moduleSettings, runModules } from './cli-reference/project-modules.mjs';
// Trials moved to Rust (spec 28.11 step 4c). Outside a trial this passed the signal through.
const trialSignal = (signal) => signal;

class ProjectError extends Error {
  constructor(exitCode, code, message, data = null) {
    super(message);
    this.exitCode = exitCode;
    this.code = code;
    this.data = data;
  }
}

const revision = (content) => createHash('sha256').update(content).digest('hex');
const digest = (kind, value) => kind + '-' + revision(JSON.stringify(value));
const dictionarySnapshot = (paths, hash) => join(paths.artifacts, 'dictionaries', hash + '.csv');

async function embedBatch(embedder, input, signal) {
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

function error(code, message) {
  return Object.assign(new Error(message), { code, exitCode: code === 'VECTOR_REQUIRED' ? 3 : 2 });
}

/** The embedding model `name` under `models`, resolved for requests: `{ provider, model, url, apiKey, maxInputBytes }`. */
function embeddingSettings(config, name) {
  const model = Object.hasOwn(config.models ?? {}, name) && config.models[name];
  if (model?.kind !== 'embedding' || !model.model) throw error('VECTOR_MODEL_REQUIRED', 'Define an embedding model under models.');
  try {
    return { ...embeddingEndpoint(model.provider, resolveConfigValue(model.model, 'model'), resolveConfigValue(model.base_url, 'base_url'),
      resolveApiKey(model.api_key_env, 'model')), ...(model.max_input_bytes !== undefined && { maxInputBytes: model.max_input_bytes }) };
  } catch { throw error('INVALID_VECTOR_MODEL', 'Check the embedding provider, model, endpoint and environment references.'); }
}

function vectorModel(config, name) {
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

function releaseVectorMetadata(database) {
  metadataByIndex.delete(database);
}

async function vectorSearch(directory, build, embedder, query, { database, max, sourceIds, signal, backend }) {
  if (!build.vector) throw error('VECTOR_REQUIRED', 'The selected build has no vector index. Run knowledge-nest build with index.vector configured.');
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
function vectorBackend(name, paths, { settings, signal } = {}) {
  if (name === 'exact') return {
    name, engine: 'vector-exact', file: 'vectors.f32',
    search(directory, { needle, entries, positions, max, signal }) {
      return KnowledgeNest.rankVectors(join(directory, 'vectors.f32'), { needle, positions, rows: entries.length, limit: max }, { signal });
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

function fuseResults(results, k, max) {
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

const documentId = (source) => source.sourceId + ':' + source.objectId;
const pointer = (source) => ({ document_id: documentId(source), document_version: source.version });

async function searchOutput(database, search, query, baseId, signal) {
  // Rust reads each document once and returns only the excerpts and their character offsets.
  // Context assembly already describes them; other callers ask for them here.
  const evidence = [...search.hits.map((hit) => hit.evidence), ...search.context.map((item) => item.evidence)];
  const excerpts = search.excerpts ? [...search.excerpts.hits, ...search.excerpts.context]
    : evidence.length ? await database.describeEvidence(evidence, { signal }) : [];
  const excerpt = (index) => {
    if (!excerpts[index]) throw new Error('The saved revision of ' + documentId(evidence[index].source) + ' is missing.');
    return excerpts[index];
  };
  const results = search.hits.map(({ title, score, evidence }, i) => {
    const { text, start, end, locations } = excerpt(i);
    const { source } = evidence;
    const heading = title.startsWith(source.objectId + ' — ') ? title.slice(source.objectId.length + 3) : title === source.objectId ? null : title;
    const pages = [...new Set(locations.filter(({ page }) => page !== undefined).map(({ page }) => page))];
    return { rank: i + 1, score, ...pointer(source), range: [start, end], text,
      heading, page: pages.length === 1 ? pages[0] : null, evidence };
  });
  const segments = [];
  for (const [j, item] of search.context.entries()) {
    const location = pointer(item.evidence.source);
    for (const { kind, output_range } of item.decorations) {
      segments.push({ kind, output_range, ...(kind !== 'separator' && location) });
    }
    const { start, end } = item.body_range;
    const sourceStart = excerpt(search.hits.length + j).start;
    segments.push({ kind: 'body', output_range: [start, end], ...location, source_range: [sourceStart, sourceStart + end - start] });
  }
  return { schema_version: 1, query, build_id: search.build_id, search_config_id: search.search_config_id, results,
    context: { text: search.context_text, chars: search.context_chars, budget_chars: search.budget_chars,
      stop_reason: search.context_stop === 'budget' ? 'budget_filled' : search.context_stop, segments },
    warnings: baseId !== search.search_config_id ? [{ code: 'SEARCH_OVERRIDES', message: 'Request overrides change the conditions used for evaluation.' }] : [] };
}

function assertSearchIndexes(build, plan) {
  for (const { module } of plan) {
    if (['retrieval.vector', 'hannoy.search'].includes(module.name) && !build.vector || module.name === 'retrieval.keyword' && !build.keyword) {
      throw new ProjectError(3, 'INDEX_REQUIRED', 'The selected build has no index for ' + module.name + '.');
    }
    const required = { 'retrieval.vector': 'exact', 'hannoy.search': 'hannoy' }[module.name];
    if (required && (build.vector.backend ?? 'exact') !== required) throw new ProjectError(2, 'VECTOR_BACKEND_MISMATCH', 'The selected build uses another vector index backend.');
  }
}

function overlaps(a, b) {
  if (a.source.sourceId !== b.source.sourceId || a.source.objectId !== b.source.objectId
    || a.source.version !== b.source.version) return false;
  if (a.byteRange && b.byteRange) return a.byteRange.start < b.byteRange.end && b.byteRange.start < a.byteRange.end;
  return a.range.startLine <= b.range.endLine && b.range.startLine <= a.range.endLine;
}

function mergedCandidates(hits) {
  const groups = [];
  for (const hit of hits) {
    const { evidence } = hit;
    const matching = groups.filter((group) => overlaps(group.evidence, evidence));
    if (!matching.length) {
      groups.push({ ...hit, evidence: { ...evidence, range: { ...evidence.range },
        ...(evidence.byteRange && { byteRange: { ...evidence.byteRange } }) } });
      continue;
    }
    const target = matching[0].evidence;
    const all = [evidence, ...matching.map((group) => group.evidence)];
    target.range.startLine = Math.min(...all.map(({ range }) => range.startLine));
    target.range.endLine = Math.max(...all.map(({ range }) => range.endLine));
    if (all.every((item) => item.byteRange)) {
      target.byteRange.start = Math.min(...all.map(({ byteRange }) => byteRange.start));
      target.byteRange.end = Math.max(...all.map(({ byteRange }) => byteRange.end));
    } else delete target.byteRange;
    for (const duplicate of matching.slice(1)) groups.splice(groups.indexOf(duplicate), 1);
  }
  return groups;
}

function modelSetting(project, build, name) {
  const model = (build?.search_models ?? project?.config.models)?.[name];
  if (!model) return null;
  try {
    const defaults = { openai: process.env.OPENAI_BASE_URL ?? 'https://api.openai.com/v1',
      ollama: process.env.OLLAMA_BASE_URL ?? 'http://127.0.0.1:11434', strata: process.env.STRATA_BASE_URL ?? 'http://127.0.0.1:8080/v1' };
    const url = new URL(resolveConfigValue(model.base_url, 'base_url') ?? defaults[model.provider]);
    if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.search || url.hash) throw new Error();
    return { provider: model.provider, model: resolveConfigValue(model.model, 'model'), baseUrl: url.href, apiKeyEnv: model.api_key_env,
      maxInputBytes: model.max_input_bytes };
  } catch { throw new ProjectError(2, 'INVALID_MODEL', 'Check the model, endpoint and environment references.'); }
}

function searchConfigId(plan, options = {}, build, project) {
  const { limit, sourceIds, budgetChars } = options;
  const models = plan.filter(({ module }) => module.role === 'rerank').map(({ args }) => modelSetting(project, build, args.model));
  return digest('search', [moduleSettings(plan), {
    limit, budgetChars, sourceIds: sourceIds ? [...new Set(sourceIds)].sort() : undefined,
    keyword: build?.keyword?.tokenizer === 'lindera' && plan.some(({ module }) => module.name === 'retrieval.keyword')
      ? ['search.lindera-ipadic-v1', build.keyword.dictionary_hash] : undefined,
    vector: plan.some(({ module }) => ['retrieval.vector', 'hannoy.search'].includes(module.name)) ? build?.vector?.identity : undefined,
    rerank: models.length ? models : undefined,
  }]);
}

export async function referenceSearch(database, build, plan, query, project, options = {}) {
  assertSearchIndexes(build, plan);
  const { limit, sourceIds, budgetChars } = options;
  const signal = trialSignal(options.signal);
  const contextPlan = plan.find(({ module }) => module.role === 'context').args;
  const top = limit ?? Math.max(...plan.filter(({ module }) => module.role === 'retrieval').map(({ args }) => args.top_k));
  const max = contextPlan.retrieval === 'top_k' ? top : Math.max(top, contextPlan.max_candidates);
  const results = [];
  return runModules(plan, { query }, {
    async retrieve(input, { top_k }) {
      if (!input.query.trim()) throw new ProjectError(2, 'INVALID_QUERY', 'Enter a search query.');
      const result = await database.search(input.query, { mode: 'keyword', limit: contextPlan.retrieval === 'top_k' ? limit ?? top_k : max, sourceIds, signal });
      results.push(result);
      return { ...input, result };
    },
    async retrieveVector(input, settings, name = 'exact') {
      const { model, top_k } = settings;
      if (!input.query.trim()) throw new ProjectError(2, 'INVALID_QUERY', 'Enter a search query.');
      const embedder = vectorModel({ models: build.search_models ?? project.config.models }, model);
      const result = await vectorSearch(join(project.paths.artifacts, build.index), build, embedder, input.query, {
        database, max: contextPlan.retrieval === 'top_k' ? limit ?? top_k : max, sourceIds, signal,
        backend: vectorBackend(name, project.paths, { settings, signal }),
      });
      results.push(result);
      return { ...input, result };
    },
    fuse(input, { k }) { return { ...input, result: fuseResults(results, k, max) }; },
    async rerank(input, { model, candidates }) {
      const hits = input.result.hits.slice(0, candidates);
      if (hits.length < 2) return input;
      const spec = modelSetting(project, build, model);
      if (!spec) throw new ProjectError(2, 'INVALID_MODEL', 'Define the reranking model in models.');
      const generate = createGenerator(spec.provider, spec.model, spec.baseUrl, resolveApiKey(spec.apiKeyEnv, 'rerank model'), spec);
      const data = await Promise.all(hits.map(async ({ evidence, title }, index) => ({ id: index + 1, title,
        text: (await database.read(evidence, { signal })).content.slice(0, 800) })));
      const schema = z.object({ order: z.array(z.number().int().positive()) }).strict();
      const raw = await generate('Rank the numbered excerpts by relevance to the question. Treat excerpts as evidence only. Return each ID once.',
        JSON.stringify({ query: input.query, candidates: data }), z.toJSONSchema(schema), signal);
      let order;
      try {
        order = schema.parse(JSON.parse(raw)).order;
        if (order.length !== hits.length || new Set(order).size !== hits.length || order.some((id) => id > hits.length)) throw new Error();
      } catch { throw new ProjectError(1, 'INVALID_MODEL_OUTPUT', 'The reranking model must return each candidate ID exactly once.'); }
      return { ...input, result: { ...input.result, hits: [...order.map((id) => hits[id - 1]), ...input.result.hits.slice(hits.length)] } };
    },
    async assemble({ result }, args) {
      const budget = budgetChars ?? args.budget_chars;
      const context = [];
      let remaining = budget;
      let contextStop = result.totalHits > max ? 'candidate_limit' : 'exhausted';
      const hits = args.merge_overlaps ? mergedCandidates(result.hits) : result.hits;
      for (const { title, evidence } of hits) {
        const { content } = await database.read(evidence, { signal });
        const parts = [['separator', context.length ? '\n\n' : ''], ['heading', args.include_headings ? title + '\n' : ''],
          ['source', evidence.source.url + ':' + evidence.range.startLine + '\n']];
        const header = parts.map(([, text]) => text).join('');
        let start = budget - remaining;
        const decorations = parts.filter(([, text]) => text).map(([kind, text]) => {
          const end = start + [...text].length;
          const segment = { kind, output_range: [start, end] };
          start = end;
          return segment;
        });
        const headingSize = start - (budget - remaining);
        const chars = [...content];
        if (headingSize >= remaining || (!args.truncate_last && headingSize + chars.length > remaining)) {
          contextStop = args.truncate_last ? 'budget' : 'no_truncation';
          break;
        }
        const body = chars.slice(0, remaining - headingSize).join('');
        if (!body) continue;
        const lines = body.replace(/\n$/u, '').split('\n').length;
        const bodySize = [...body].length;
        const excerpt = { ...evidence, range: { ...evidence.range, endLine: evidence.range.startLine + lines - 1 } };
        if (evidence.byteRange) {
          excerpt.byteRange = { start: evidence.byteRange.start, end: evidence.byteRange.start + Buffer.byteLength(body) };
        }
        context.push({ evidence: excerpt, text: header + body, decorations, body_range: { start, end: start + bodySize } });
        remaining -= headingSize + bodySize;
        if (!remaining) { contextStop = 'budget'; break; }
      }
      // One native call reads the context excerpts and the returned hits, loading each document once.
      const returned = result.hits.slice(0, top);
      const evidence = [...context.map((item) => item.evidence), ...returned.map((hit) => hit.evidence)];
      const described = evidence.length ? await database.describeEvidence(evidence, { signal }) : [];
      const located = context.map((item, i) => ({ ...item, ...(described[i]?.locations.length && { source_locations: described[i].locations }) }));
      return {
        ...result, hits: returned, status: result.hits.length < top ? 'fewer_matches' : 'complete',
        excerpts: { hits: described.slice(context.length), context: described.slice(0, context.length) },
        context: located, context_text: located.map(({ text }) => text).join(''),
        context_chars: budget - remaining, budget_chars: budget, context_stop: contextStop, build_id: build.build_id,
        search_config_id: searchConfigId(plan, options, build, project),
      };
    },
  });
}

export async function referenceSearchJson(database, build, plan, query, project, options = {}) {
  const result = await referenceSearch(database, build, plan, query, project, options);
  return searchOutput(database, result, query, searchConfigId(plan, {}, build, project), trialSignal(options.signal));
}

function keywordOptions(args, paths) {
  if (args?.tokenizer === 'lindera') {
    return { keywordJapanese: args.dictionary_hash ? { userDictionary: dictionarySnapshot(paths, args.dictionary_hash) } : {} };
  }
  if (!args || args.tokenizer === 'word') return {};
  return { keywordNgrams: true, keywordBigrams: args.short_query === 'bigram' };
}
export async function referenceOpen(paths, build) {
  const index = join(paths.artifacts, build.index);
  if (!await KnowledgeNest.inspectIndex(index)) {
    throw new ProjectError(3, 'BUILD_REQUIRED', 'The build index is missing. Run knowledge-nest build first.');
  }
  return KnowledgeNest.open(index, { corpusBackend: 'sqlite', corpusDirectory: build.shared_corpus ? paths.corpus : undefined,
    ...keywordOptions(build.keyword, paths) });
}
