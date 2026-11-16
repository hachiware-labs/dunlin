// The JavaScript index stage before it moved to Rust (spec 28.9 step 2c): the keyword index, the
// vector index with the embedding cache, the hannoy tool and the build record in state.sqlite.
// Kept to check that the Rust version keeps build IDs, files and state unchanged. The project
// settings come in already resolved, as the Rust request takes them.
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { constants } from 'node:fs';
import { chmod, copyFile, cp, link, lstat, mkdir, readFile, readdir, realpath, rename, rm, stat, writeFile } from 'node:fs/promises';
import { basename, dirname, join } from 'node:path';
import { Dunlin } from 'dunlin';

import { embedderFor } from '../../models.mjs';
import { embedBatch } from '../../vectors.mjs';

const revision = (content) => createHash('sha256').update(content).digest('hex');
const digest = (kind, value) => kind + '-' + revision(JSON.stringify(value));
const failure = (exitCode, code, message, data = null) => Object.assign(new Error(message), { exitCode, code, data });
const vectorError = (code, message) => Object.assign(new Error(message), { code, exitCode: code === 'VECTOR_REQUIRED' ? 3 : 2 });
const toolError = (code, message, exitCode = 2) => Object.assign(new Error(message), { code, exitCode });

async function exists(path) {
  try { await stat(path); return true; } catch { return false; }
}

async function atomicWrite(path, content) {
  const temporary = `${path}.${process.pid}-${Date.now()}.tmp`;
  try {
    await writeFile(temporary, content, { flag: 'wx' });
    await rename(temporary, path);
  } finally {
    await rm(temporary, { force: true });
  }
}

function readStage(db, stage, id) {
  const row = db.prepare('SELECT manifest FROM stage_outputs WHERE stage = ? AND id = ?').get(stage, id);
  if (!row) return null;
  const output = JSON.parse(row.manifest);
  if (output.payloads !== 1) return output;
  const find = db.prepare('SELECT data FROM stage_payloads WHERE id = ?');
  const get = (key) => {
    const row = find.get(key);
    if (!row) throw failure(3, 'STAGE_REQUIRED', 'A saved stage payload is missing.');
    return JSON.parse(row.data);
  };
  if (stage === 'source') output.groups = output.groups.map(([source, docs]) => [source, docs.map(({ text, ...doc }) => ({ ...doc, content: get(text) }))]);
  if (stage === 'transform') output.chunks = output.chunks.map((group) => group.map(get));
  return output;
}

async function readUserDictionary(path) {
  if (!path) return null;
  try {
    const bytes = await readFile(path);
    return { bytes, hash: revision(bytes) };
  } catch {
    throw failure(2, 'USER_DICTIONARY_REQUIRED', 'Could not read the user dictionary CSV at ' + path);
  }
}

const dictionarySnapshot = (paths, hash) => join(paths.artifacts, 'dictionaries', hash + '.csv');

function keywordOptions(args, paths) {
  if (args?.tokenizer === 'lindera') {
    return { keywordJapanese: args.dictionary_hash ? { userDictionary: dictionarySnapshot(paths, args.dictionary_hash) } : {} };
  }
  if (!args || args.tokenizer === 'word') return {};
  return { keywordNgrams: true, keywordBigrams: args.short_query === 'bigram' };
}

async function copyIndexSnapshot(source, target) {
  const keywordDir = '.dunlin-index-v1';
  const from = join(source, keywordDir);
  const to = join(target, keywordDir);
  const options = { recursive: true, mode: constants.COPYFILE_FICLONE,
    filter: (path) => !path.endsWith('-shm') && !/vectors\.(json|f32)$/u.test(path) && basename(path) !== 'hannoy' };
  await cp(source, target, { ...options, filter: (path) => path !== from && options.filter(path) });

  await mkdir(to);
  for (const entry of await readdir(from, { withFileTypes: true })) {
    const input = join(from, entry.name);
    const output = join(to, entry.name);
    if (entry.isFile() && /^[0-9a-f]{32}\.(?:idx|pos|term|store|fast|fieldnorm)$/u.test(entry.name)) {
      try {
        await link(input, output);
        continue;
      } catch (error) {
        if (!['EACCES', 'EINVAL', 'EMLINK', 'ENOSYS', 'ENOTSUP', 'EOPNOTSUPP', 'EPERM', 'EXDEV'].includes(error.code)) throw error;
      }
    }
    await cp(input, output, options);
  }
  await chmod(to, (await lstat(from)).mode);
}

function callTool(binary, request, signal) {
  signal?.throwIfAborted();
  return new Promise((resolve, reject) => {
    const env = Object.fromEntries(['SystemRoot', 'WINDIR', 'TMP', 'TEMP', 'TMPDIR'].flatMap((key) =>
      process.env[key] === undefined ? [] : [[key, process.env[key]]]));
    const child = spawn(binary, { windowsHide: true, stdio: ['pipe', 'pipe', 'ignore'], env, signal });
    let output = '';
    child.stdout.setEncoding('utf8');
    child.stdout.on('data', (chunk) => {
      output += chunk;
      if (output.length > 1024 * 1024) {
        child.kill();
        reject(toolError('TOOL_PROTOCOL', 'The index tool response exceeded its size limit.', 1));
      }
    });
    child.on('error', () => reject(signal?.aborted ? signal.reason : toolError('TOOL_EXECUTION', 'Could not start the index tool.', 1)));
    child.stdin.on('error', () => {});
    child.on('close', (code) => {
      if (code !== 0) {
        reject(signal?.aborted ? signal.reason : toolError('TOOL_EXECUTION', 'The index tool failed during ' + request.operation + '.', 1));
        return;
      }
      try { resolve(JSON.parse(output)); }
      catch { reject(toolError('TOOL_PROTOCOL', 'The index tool returned invalid JSON.', 1)); }
    });
    child.stdin.end(JSON.stringify(request));
  });
}

async function buildVectorIndex(state, database, path, source, transformed, embedder, force, backend, signal) {
  const identity = revision(JSON.stringify(embedder.identity));
  const lookup = state.prepare('SELECT data FROM embeddings WHERE id = ?');
  const save = state.prepare(force ? 'INSERT OR REPLACE INTO embeddings VALUES (?, ?)' : 'INSERT OR IGNORE INTO embeddings VALUES (?, ?)');
  const entries = [];
  const pending = new Map();
  for (const [group, [sourceId, documents]] of source.groups.entries()) {
    for (const [i, document] of documents.entries()) {
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
  if ([...vectors.values()].some((value) => value.length !== width)) throw vectorError('VECTOR_DIMENSIONS', 'Embedding dimensions differ. Choose a fixed version of the model and rebuild.');
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

function vectorBackend(name, { settings, baseline, signal, toolPath } = {}) {
  if (name === 'exact') return {
    name, file: 'vectors.f32',
    async build(directory, { entries }, bytes) {
      await writeFile(join(directory, 'vectors.f32'), bytes);
      return { indexed: entries.length, removed: 0 };
    },
  };
  if (name !== 'hannoy') throw vectorError('VECTOR_BACKEND_MISMATCH', 'Unknown vector index backend.');
  return {
    name, file: join('hannoy', 'data.mdb'),
    async build(directory, metadata, bytes) {
      const { identity, dimensions, entries } = metadata;
      const location = join(directory, 'hannoy');
      if (!entries.length) return { indexed: 0, removed: 0 };
      await mkdir(location);
      const tool = await toolPath();
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
        const { inserted, deleted } = await callTool(tool, { operation: 'build', directory: location, vectors: file, dimensions, rows: entries.length,
          inserts, deletes: [...prior.values()], ef_construction: settings.ef_construction }, signal);
        if (inserted !== inserts.length || deleted !== prior.size) throw vectorError('VECTOR_BACKEND_MISMATCH', 'The index tool reported an incomplete build.');
        return { indexed: inserted, removed: deleted, reused: entries.length - inserted, identity };
      } finally { await rm(file, { force: true }); }
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

/**
 * `request` is the Rust request (IndexStageRequest). `host.configChanged()` and `host.indexTool()`
 * answer as the Rust host calls do. Returns `{ output, warnings }`.
 */
export async function referenceIndexStage(db, request, host) {
  const { runId, keyword: args, vector: vectorArgs, search, force, activate, tag, generation } = request;
  const paths = { artifacts: request.artifacts, corpus: request.corpus };
  const warnings = [];
  const transformed = readStage(db, 'transform', request.transformId);
  const source = readStage(db, 'source', transformed.source_id);
  if (!source) throw failure(3, 'STAGE_REQUIRED', 'The source output referenced by transform is missing. Run dunlin build.');
  const embedder = vectorArgs ? embedderFor(request.embedder) : null;
  const dictionary = await readUserDictionary(args?.user_dictionary);
  const japanese = args?.tokenizer === 'lindera';
  const keyword = japanese ? { tokenizer: 'lindera', dictionary_hash: dictionary?.hash ?? null } : args;
  const indexConfigId = digest('cfg', [japanese ? 'index.lindera-ipadic-v1' : 'index.keyword-v2', keyword,
    ...(embedder ? ['index.vector-v1', vectorArgs, embedder.identity] : [])]);
  const configId = digest('cfg', [source.config_id, transformed.config_id, indexConfigId, search, request.rerank]);
  const buildKey = digest('bld', [transformed.id, indexConfigId]);
  const cached = embedder && db.prepare('SELECT build_id FROM index_outputs WHERE key = ?').get(buildKey);
  let buildId = cached?.build_id ?? buildKey;
  const saved = db.prepare('SELECT manifest FROM builds WHERE id = ?').get(buildId);
  let manifest = saved && JSON.parse(saved.manifest);
  const backend = vectorArgs && vectorBackend(vectorArgs.backend);
  const reused = Boolean(!force && manifest && await Dunlin.inspectIndex(join(paths.artifacts, manifest.index))
    && (!embedder || await exists(join(paths.artifacts, manifest.index, 'vectors.json'))
      && (manifest.vector?.count === 0 || await exists(join(paths.artifacts, manifest.index, backend.file)))));
  const previous = db.prepare('SELECT build_id FROM project WHERE id = 1').get().build_id;
  const configuration = request.configuration;
  const existingTag = tag && db.prepare('SELECT build_id FROM build_tags WHERE name = ?').get(tag);
  const tagSettings = tag && db.prepare('SELECT settings FROM tag_search WHERE name = ?').get(tag);
  if (existingTag && (tagSettings?.settings !== JSON.stringify(search) || reused && existingTag.build_id !== buildId)) {
    throw failure(2, 'TAG_EXISTS', 'Build tag already names a different configuration: ' + tag);
  }
  const location = join(paths.artifacts, runId);
  let created = false;
  let committed = false;
  let changed = false;
  try {
    if (dictionary) {
      const snapshot = dictionarySnapshot(paths, dictionary.hash);
      await mkdir(dirname(snapshot), { recursive: true });
      if (!await exists(snapshot)) await atomicWrite(snapshot, dictionary.bytes);
    }
    if (!reused) {
      await mkdir(location);
      created = true;
      const active = db.prepare('SELECT manifest FROM builds WHERE id = ?').get(previous);
      const prior = active && JSON.parse(active.manifest);
      const baseline = !force && prior?.shared_corpus && JSON.stringify(prior.keyword) === JSON.stringify(keyword)
        && await Dunlin.inspectIndex(join(paths.artifacts, prior.index));
      if (baseline) {
        await copyIndexSnapshot(join(paths.artifacts, prior.index), location);
      }
      const database = await Dunlin.open(location, { corpusBackend: 'sqlite', corpusDirectory: paths.corpus, ...keywordOptions(keyword, paths) })
        .catch((error) => {
          if (dictionary && error.code === 'INVALID_ARGUMENT') throw failure(2, 'INVALID_USER_DICTIONARY',
            'Could not load ' + args.user_dictionary + '. ' + error.message);
          throw error;
        });
      let vector;
      try {
        const unused = new Set(Object.keys(baseline ? baseline.sources : {}));
        for (const [group, [id, documents]] of source.groups.entries()) {
          const input = documents.map((document, i) => ({ ...document, chunks: transformed.chunks[group][i] }));
          if (baseline) await database.syncDocuments(id, input);
          else await database.replaceDocuments(id, input);
          unused.delete(id);
        }
        for (const id of unused) await database.removeSource(id);
        if (embedder) {
          const compatible = !force && prior?.vector?.backend === vectorArgs.backend && prior.vector.ef_construction === vectorArgs.settings.ef_construction;
          const index = vectorBackend(vectorArgs.backend, { settings: vectorArgs.settings,
            baseline: compatible ? join(paths.artifacts, prior.index) : null, toolPath: () => host.indexTool('hannoy') });
          vector = await buildVectorIndex(db, database, location, source, transformed, embedder, force, index);
          vector.ef_construction = vectorArgs.settings.ef_construction;
        }
      } finally {
        await database.close();
      }
      const { searchableDocuments } = await Dunlin.inspectIndex(location);
      if (!searchableDocuments) {
        throw failure(3, 'NO_SEARCHABLE_DOCUMENTS', 'No searchable documents. Add UTF-8 text or Markdown files and run dunlin build.', {
          files: source.files, documents: 0, skipped: source.skipped,
        });
      }
      if (vector) buildId = digest('bld', [buildKey, vector.output]);
      manifest = {
        build_id: buildId, index: runId, files: source.files, documents: searchableDocuments,
        skipped: source.skipped, source_id: source.id, transform_id: transformed.id, keyword, vector, config_id: configId,
        construction: { source: source.modules, transform: transformed.modules, index: request.construction }, shared_corpus: true,
      };
    }
    db.exec('BEGIN IMMEDIATE');
    try {
      const currentTag = tag && db.prepare('SELECT build_id, settings FROM build_tags JOIN tag_search USING (name) WHERE name = ?').get(tag);
      if (currentTag && (currentTag.build_id !== buildId || currentTag.settings !== JSON.stringify(search))) {
        throw failure(2, 'TAG_EXISTS', 'Build tag already names a different configuration: ' + tag);
      }
      if (!reused) {
        db.prepare('INSERT INTO builds VALUES (?, ?) ON CONFLICT(id) DO UPDATE SET manifest = excluded.manifest')
          .run(buildId, JSON.stringify(manifest));
      }
      if (embedder) db.prepare('INSERT OR REPLACE INTO index_outputs VALUES (?, ?)').run(buildKey, buildId);
      if (activate && (generation !== db.prepare('SELECT max(id) AS id FROM adoption_history').get().id
        || await host.configChanged())) {
        warnings.push({ code: 'ADOPTION_CHANGED', message: 'The adopted configuration changed during the build. The serving build was kept.' });
      } else if (activate) {
        db.prepare('UPDATE project SET build_id = ? WHERE id = 1').run(buildId);
        db.prepare('INSERT INTO serving_generations VALUES (?, ?) ON CONFLICT(id) DO UPDATE SET build_id = excluded.build_id')
          .run(generation ?? 0, buildId);
        changed = previous !== buildId || !reused;
      }
      db.prepare('INSERT OR IGNORE INTO build_search VALUES (?, ?)').run(buildId, JSON.stringify(search));
      db.prepare('INSERT OR IGNORE INTO build_config VALUES (?, ?)').run(buildId, JSON.stringify(configuration));
      if (tag && !currentTag) {
        db.prepare('INSERT INTO build_tags VALUES (?, ?)').run(tag, buildId);
        db.prepare('INSERT INTO tag_search VALUES (?, ?)').run(tag, JSON.stringify(search));
        db.prepare('INSERT INTO build_config VALUES (?, ?)').run('tag:' + tag, JSON.stringify(configuration));
        db.prepare('INSERT INTO tag_history (name, previous, target) VALUES (?, ?, ?)').run(tag, null,
          JSON.stringify({ build_id: buildId, search }));
      }
      db.prepare('INSERT INTO build_runs VALUES (?, ?, ?, ?)').run(runId, buildId, configId, new Date().toISOString());
      db.exec('COMMIT');
      committed = true;
    } catch (error) {
      db.exec('ROLLBACK');
      throw error;
    }
    const { index: _, ...summary } = manifest;
    return { output: { ...summary, config_id: configId, serving_changed: changed, reused }, warnings };
  } finally {
    if (created && !committed) {
      const root = await realpath(request.root);
      const target = await realpath(location);
      if (target === join(root, '.dunlin', 'artifacts', runId)) {
        await rm(target, { recursive: true, force: true, maxRetries: 3 });
      }
    }
  }
}
