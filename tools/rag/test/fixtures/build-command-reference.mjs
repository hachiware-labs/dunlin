// The JavaScript build commands before they moved to Rust (spec 28.9 step 2d): stage selection,
// inputs from --input and --tag, the stage calls, warnings, results and --dry-run. Kept to check
// that the Rust version gives the same results. Takes the Rust request (BuildCommandRequest).
import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { DatabaseSync } from 'node:sqlite';
import { join } from 'node:path';
import { performance } from 'node:perf_hooks';
import { Dunlin } from 'dunlin';

import { inventory, parserAssignments, unsupportedFiles } from '../../project-content.mjs';

const stages = ['source', 'transform', 'index'];
const revision = (content) => createHash('sha256').update(content).digest('hex');
const digest = (kind, value) => kind + '-' + revision(JSON.stringify(value));
const failure = (exitCode, code, message, data = null) => Object.assign(new Error(message), { exitCode, code, data });

function failed({ code, message, exitCode, data }) {
  return exitCode ? failure(exitCode, code, message, data ?? null) : Object.assign(new Error(message), code && { code });
}

function readStage(db, stage, id) {
  const row = id === undefined
    ? db.prepare('SELECT manifest FROM stage_outputs JOIN latest_stage USING (stage, id) WHERE stage = ?').get(stage)
    : db.prepare('SELECT manifest FROM stage_outputs WHERE stage = ? AND id = ?').get(stage, id);
  return row ? JSON.parse(row.manifest) : null;
}

const latestStageId = (db, stage) => db.prepare('SELECT id FROM latest_stage WHERE stage = ?').get(stage)?.id;

function currentBuild(db, required = true, reference) {
  const row = reference === undefined
    ? db.prepare('SELECT manifest FROM project JOIN builds ON builds.id = project.build_id').get()
    : db.prepare(`SELECT manifest FROM builds WHERE id = ? OR id = (SELECT build_id FROM build_tags WHERE name = ?)
      ORDER BY id = ? DESC LIMIT 1`).get(reference, reference, reference);
  if (row) return JSON.parse(row.manifest);
  if (required) throw failure(3, 'BUILD_REQUIRED', 'No searchable build. Run dunlin build first.');
  return null;
}

function resolveBuildInput(db, { input, from, tag }) {
  if (input) return currentBuild(db, true, input);
  return from && tag ? currentBuild(db, false, tag) : null;
}

function inputStageId(db, stage, fixed) {
  const id = fixed ? fixed[stage + '_id'] : latestStageId(db, stage);
  if (id !== undefined && db.prepare('SELECT 1 FROM stage_outputs WHERE stage = ? AND id = ?').get(stage, id)) return id;
  if (fixed) throw failure(3, 'STAGE_REQUIRED', 'Missing saved ' + stage + ' for ' + fixed.build_id + '.');
  throw failure(3, 'STAGE_REQUIRED', 'No ' + stage + ' output. Run dunlin ' + stage + ' or dunlin build first.');
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

/** Returns `{ output, warnings }` as the Rust build does. `host` answers the stage host calls. */
export async function referenceBuild(request, host) {
  const { command } = request;
  const values = request;
  const selected = command === 'build'
    ? stages.slice(values.from === undefined ? 0 : stages.indexOf(values.from), values.until === undefined ? stages.length : stages.indexOf(values.until) + 1)
    : [command];
  const modules = { source: request.source.modules, transform: request.transform.modules, index: request.index.construction };
  if (request.dryRun) {
    const data = { stages: Object.fromEntries(selected.map((stage) => [stage, { modules: modules[stage] }])), serving_changed: false };
    const state = new DatabaseSync(request.state, { readOnly: true });
    try {
      const serving = currentBuild(state, false);
      const previous = serving ? readStage(state, 'source', serving.source_id) : readStage(state, 'source');
      const transform = serving && readStage(state, 'transform', serving.transform_id);
      if (selected[0] === 'source') {
        const { sources, routes } = request.source;
        const parsers = parserAssignments(sources, routes);
        data.skipped = unsupportedFiles(sources, routes);
        data.commands = request.source.commands.map(({ module, args }) =>
          ({ module, command: args.command, ...(args.version_command && { version_command: args.version_command }) }));
        const skipped = new Set(data.skipped.map(({ source_id, path }) => JSON.stringify([source_id, path])));
        const sourceArgsChanged = previous?.config_id !== digest('cfg', request.source.modules);
        const transformArgsChanged = transform?.config_id !== digest('cfg', request.transform.modules);
        const config = serving && state.prepare('SELECT config FROM build_config WHERE ref = ?').get(serving.build_id);
        const dictionary = selected.includes('index') && await readUserDictionary(request.index.keyword?.user_dictionary);
        const indexArgsChanged = !config || JSON.stringify(JSON.parse(config.config).index) !== JSON.stringify(request.configIndex)
          || (serving?.keyword?.dictionary_hash ?? null) !== (dictionary?.hash ?? null);
        data.files = (await inventory(sources, previous)).map((file) => {
          const skip = skipped.has(JSON.stringify([file.source_id, file.path]));
          const changed = request.force || file.change !== 'unchanged';
          return { ...file, parser: parsers.get(JSON.stringify([file.source_id, file.path])) ?? null, stages: Object.fromEntries(selected.map((stage) => [stage,
            skip ? 'skip' : file.change === 'deleted' ? 'remove' : changed ? 'run'
              : stage === 'source' ? sourceArgsChanged ? 'check_cache' : 'reuse'
              : stage === 'transform' ? sourceArgsChanged || transformArgsChanged ? 'check_cache' : 'reuse'
              : sourceArgsChanged || transformArgsChanged || indexArgsChanged ? 'check_cache' : 'reuse',
          ])) };
        });
      } else {
        const fixed = resolveBuildInput(state, request);
        data.input_id = inputStageId(state, stages[stages.indexOf(selected[0]) - 1], fixed);
      }
    } finally { state.close(); }
    return { output: data, warnings: [] };
  }
  const start = performance.now() - (request.elapsedMs ?? 0);
  const warnings = [];
  const db = new DatabaseSync(request.state);
  try {
    const generation = db.prepare('SELECT max(id) AS id FROM adoption_history').get().id;
    const fixed = resolveBuildInput(db, request);
    const results = {};
    let source;
    for (const stage of selected) {
      const stageStart = performance.now();
      if (stage === 'index') {
        const transformId = inputStageId(db, 'transform', selected.includes('transform') ? null : fixed);
        const transformed = JSON.parse(db.prepare("SELECT manifest FROM stage_outputs WHERE stage = 'transform' AND id = ?").get(transformId).manifest);
        if (!selected.includes('transform') && latestStageId(db, 'source') !== transformed.source_id) {
          warnings.push({ code: 'STALE_STAGE', message: 'Using saved transform output from an earlier source snapshot.' });
        }
        const result = await Dunlin.runIndex({
          state: request.state, artifacts: request.artifacts, corpus: request.corpus, root: request.root, runId: request.runId, transformId,
          ...request.index, force: Boolean(request.force), tag: request.tag, generation,
          activate: command === 'build' && request.until === undefined && !request.candidate && !request.tag,
        }, host);
        if (result.status === 'failed') throw failed(result.error);
        warnings.push(...result.warnings);
        const summary = result.output;
        results.index = { id: summary.build_id, reused: summary.reused, elapsed_ms: Math.round(performance.now() - stageStart) };
        return { output: { ...summary, stages: results, elapsed_ms: Math.round(performance.now() - start) }, warnings };
      }
      let output;
      let reused;
      if (stage === 'source') {
        const { sources, routes, reject, commands, modules: sourceModules, ...ocr } = request.source;
        let result;
        try {
          result = await Dunlin.runSource({ state: request.state, originals: join(request.artifacts, 'originals', 'shared'), root: request.root,
            force: Boolean(request.force), skipUnsupported: Boolean(request.skipUnsupported), reject: Boolean(reject), ...ocr,
            sources, routes, commands, modules: sourceModules }, host);
        } catch (error) {
          if (error.code === 'LOAD_FAILED') throw new Error(error.message, { cause: error });
          throw error;
        }
        if (result.status === 'unsupported') {
          throw failure(2, 'UNSUPPORTED_FILES', 'Unsupported files found. Set source.filesystem with.exclude or use --skip-unsupported.', result.stats);
        }
        warnings.push(...result.warnings);
        ({ output, reused } = result);
        source = output;
      } else {
        const result = await Dunlin.runTransform({ state: request.state, sourceId: source?.id ?? inputStageId(db, 'source', fixed),
          force: Boolean(request.force), modules: request.transform.modules, embedder: request.transform.embedder }, host);
        if (result.status === 'failed') throw failed(result.error);
        ({ output, reused } = result);
      }
      results[stage] = { id: output.id, reused, elapsed_ms: Math.round(performance.now() - stageStart) };
    }
    return { output: { stages: results, ...(source && { files: source.files, skipped: source.skipped }), serving_changed: false,
      elapsed_ms: Math.round(performance.now() - start) }, warnings };
  } finally {
    db.close();
  }
}

/** `tagBuild` before it moved to Rust. Returns `{ tag, build_id, search_config_id }`. */
export async function referenceTag(db, { name, reference: ref, move, search, configSearch, configModels }, host) {
  db.exec('BEGIN IMMEDIATE');
  try {
    const selected = ref ?? currentBuild(db).build_id;
    const build = currentBuild(db, true, selected);
    const tag = db.prepare('SELECT settings FROM tag_search WHERE name = ?').get(selected);
    const stored = tag ?? db.prepare('SELECT settings FROM build_search WHERE build_id = ?').get(build.build_id);
    if (stored) build.search = JSON.parse(stored.settings);
    const saved = db.prepare('SELECT config FROM build_config WHERE ref = ?').get(tag ? 'tag:' + selected : build.build_id);
    if (saved) build.search_models = JSON.parse(saved.config).models;
    if (ref === undefined) build.search = search;
    const byTag = db.prepare('SELECT config FROM build_config WHERE ref = ?').get('tag:' + ref);
    const config = byTag ?? db.prepare('SELECT config FROM build_config WHERE ref = ?').get(build.build_id);
    if (!config) throw failure(3, 'CONFIG_REQUIRED', 'Build the candidate again to save its configuration.');
    const configuration = JSON.parse(config.config);
    if (ref === undefined) {
      configuration.search = configSearch;
      configuration.models = configModels;
    }
    const existing = db.prepare('SELECT build_id, settings FROM build_tags JOIN tag_search USING (name) WHERE name = ?').get(name);
    const settings = JSON.stringify(build.search);
    if (existing && (existing.build_id !== build.build_id || existing.settings !== settings) && !move) {
      throw failure(2, 'TAG_EXISTS', 'Use --move to explicitly change an existing tag.');
    }
    if (!existing || existing.build_id !== build.build_id || existing.settings !== settings) {
      db.prepare('INSERT OR REPLACE INTO build_tags VALUES (?, ?)').run(name, build.build_id);
      db.prepare('INSERT OR REPLACE INTO tag_search VALUES (?, ?)').run(name, settings);
      db.prepare('INSERT OR REPLACE INTO build_config VALUES (?, ?)').run('tag:' + name, JSON.stringify(configuration));
      db.prepare('INSERT INTO tag_history (name, previous, target) VALUES (?, ?, ?)').run(name,
        existing ? JSON.stringify({ build_id: existing.build_id, search: JSON.parse(existing.settings) }) : null,
        JSON.stringify({ build_id: build.build_id, search: build.search }));
    }
    const searchId = await host.searchConfigId(build);
    db.exec('COMMIT');
    return { tag: name, build_id: build.build_id, search_config_id: searchId };
  } catch (error) { db.exec('ROLLBACK'); throw error; }
}
