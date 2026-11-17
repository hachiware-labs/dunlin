import { randomUUID } from 'node:crypto';
import { lstat, mkdir, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import { once } from 'node:events';
import { homedir } from 'node:os';
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { parseArgs } from 'node:util';
import { performance } from 'node:perf_hooks';
import { fileURLToPath } from 'node:url';

import { Dunlin } from 'dunlin';
import { z } from 'zod';

import { atomicWrite, categories, revision, withWriteLock } from './authoring/files.mjs';
import { resolveApiKey, resolveConfigValue } from './config.mjs';
import { createGenerator } from './models.mjs';
import { chunkCorpus, ocrRequest, parserHost } from './project-content.mjs';
import { parserRoutes } from './project-parsers.mjs';
import { dictionaryEvaluation, dictionaryMisses, ensureEvaluation, evaluate, evaluationStatus, evidenceMaps, manageEvaluationData, reports } from './project-eval.mjs';
import { addDictionary, dictionaryText, suggestDictionary } from './project-dictionary.mjs';
import { ensureWiki, runWiki, serveWiki, validateWiki } from './project-wiki.mjs';
import { trackWork } from './project-wiki-work.mjs';
import { embeddingSettings } from './project-vectors.mjs';
import { changeTool, listedTools, projectOcr, toolsCatalog } from './project-tools.mjs';
import { checkIndexTool, hannoy, indexToolPath } from './project-index-tool.mjs';
import { chargeTrialApi, checkTrialLimits, runTrial, trialCheckpoint, trialLimits, trialSignal, trialUsage } from './project-trials.mjs';
import { answerModel, answerQuestion, answerText } from './project-answer.mjs';
import { configureGlobal, initialSettings } from './project-global.mjs';
import { serveRagHttp } from './project-rag-http.mjs';
import { isSearchProject, serveAddress, serveCredential, withServeOverrides } from './project-serving.mjs';
import { constructionStages as stages, describeModules, moduleSettings, parseConfig, ragTemplate, restoreModules,
  runModules, validateRag, wikiTemplate, writeTemplate } from './project-modules.mjs';

const configName = 'Dunlinfile.yaml';
let DatabaseSync;
const projectTypes = ['mcp', 'rag', 'wiki', 'para'];
export const projectCommands = {
  init: 'dunlin init <mcp|rag|wiki|para> <directory> [--source <directory> | --purpose <text>] [--embedding-model <name>] [--embedding-provider <ollama|openai>] [--llm-model <name>] [--llm-provider <ollama|openai|strata>] [--no-embedding] [--no-llm] [--ocr | --no-ocr] [--ocr-tool <oar-ocr|lightonocr>] [--json]',
  config: 'dunlin config list | dunlin config set <embedding|llm> --model <name> [--provider <name>] | dunlin config set ocr [--tool oar-ocr|lightonocr] | dunlin config unset <embedding|llm|ocr> [--json]',
  build: 'dunlin build [--project <directory>] [--from <stage>] [--input <build>] [--until <stage>] [--force] [--skip-unsupported] [--dry-run] [--config <file>] [--tag <name>] [--json]',
  source: 'dunlin source [--project <directory>] [--config <file>] [--dry-run] [--skip-unsupported] [--json]',
  transform: 'dunlin transform [--project <directory>] [--config <file>] [--dry-run] [--json]',
  index: 'dunlin index [--project <directory>] [--config <file>] [--dry-run] [--json]',
  search: 'dunlin search <query> [--project <directory>] [--build <ref>] [--config <file>] [--limit <n>] [--budget-chars <n>] [--source-id <id>] [--json]',
  ask: 'dunlin ask <question> [--project <directory>] [--build <ref>] [--config <file>] [--json]',
  serve: 'dunlin serve [--project <directory>] [--transport <http|stdio>] [--host <address>] [--port <port>] [--print-config [--json]]',
  status: 'dunlin status [--project <directory>] [--json]',
  validate: 'dunlin validate [--project <directory>] [--config <file>] [--json]',
  defaults: 'dunlin defaults [--apply <section>] [--project <directory>] [--json]',
  modules: 'dunlin modules <list|describe <name>> [--project <directory>] [--json]',
  add: 'dunlin add <tool> | dunlin add --list [--project <directory>] [--json]',
  remove: 'dunlin remove <tool> [--project <directory>] [--json]',
  use: 'dunlin use <build> [--project <directory>] [--json]',
  tag: 'dunlin tag <name> [build] [--move] [--project <directory>] [--json]',
  'eval-data': 'dunlin eval-data <add|import|export|list|show|split|generate|review|sync|diff|tag|use> [--project <directory>] [--json]',
  eval: 'dunlin eval [--project <directory>] [--build <ref>] [--config <file>] [--split dev|test] [--tag <name>] [--json]',
  report: 'dunlin report <show|list|compare> [--project <directory>] [--json]',
  dictionary: 'dunlin dictionary suggest [--report <ref>] | dunlin dictionary add <file> --config <candidate> [--project <directory>] [--json]',
  'evidence-map': 'dunlin evidence-map <create|list|show|export|import|confirm> [--project <directory>] [--build <ref>] [--json]',
  ingress: 'dunlin ingress <path...> | dunlin ingress --pending [--project <directory>] [--dry-run] [--source-only] [--json]',
  query: 'dunlin query <question> [--page <id>] [--target <text>] [--project <directory>] [--json]',
  verify: 'dunlin verify [--page <id>] [--content] [--all-issues] [--project <directory>] [--json]',
  issue: 'dunlin issue <id> <open|resolved|dismissed> [--project <directory>] [--json]',
  rules: 'dunlin rules [--project <directory>] [--json]',
  progress: 'dunlin progress <work-id> [--project <directory>] [--json]',
  cite: 'dunlin cite <page> <number> [--project <directory>] [--json]',
  instruct: 'dunlin instruct <page> <instruction> [--target <text>] [--supplement <text> --by <name>] | dunlin instruct --persistent <rule> [--project <directory>] [--json]',
  'save-answer': 'dunlin save-answer <query-run> <page> <title> [--project <directory>] [--json]',
  archive: 'dunlin archive <page> [--project <directory>] [--restore] [--json]',
  undo: 'dunlin undo <work-id> [--project <directory>] [--json]',
};

class ProjectError extends Error {
  constructor(exitCode, code, message, data = null) {
    super(message);
    this.exitCode = exitCode;
    this.code = code;
    this.data = data;
  }
}

async function exists(path) {
  return lstat(path).then(() => true, (error) => {
    if (error.code === 'ENOENT') return false;
    throw error;
  });
}

export async function findProject(directory) {
  if (directory !== undefined) return join(resolve(directory), configName);
  let current = process.cwd();
  for (;;) {
    const path = join(current, configName);
    if (await exists(path)) return path;
    const parent = dirname(current);
    if (parent === current) return null;
    current = parent;
  }
}

function projectPaths(root) {
  const data = join(root, '.dunlin');
  return { data, state: join(data, 'state.sqlite'), artifacts: join(data, 'artifacts'), corpus: join(data, 'corpus') };
}

function openState(paths, readOnly = false) {
  const db = new DatabaseSync(paths.state, { readOnly });
  db.exec('PRAGMA busy_timeout = 500');
  return db;
}

const secretSetting = /(?:api[_-]?key|password|token|secret|authorization|credential)/iu;
function safeConfig(value) {
  if (Array.isArray(value)) return value.map(safeConfig);
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).flatMap(([key, child]) => {
    if (secretSetting.test(key) && !/[_-]env$/iu.test(key)) return [];
    return [[key, safeConfig(child)]];
  }));
  if (typeof value !== 'string') return value;
  const url = URL.parse(value);
  if (!url || !['http:', 'https:'].includes(url.protocol) || (!url.username && !url.password && !url.search && !url.hash)) return value;
  url.username = '';
  url.password = '';
  url.search = '';
  url.hash = '';
  return url.href;
}

function ensureStages(db) {
  db.exec(`CREATE TABLE IF NOT EXISTS stage_outputs (
      stage TEXT NOT NULL, id TEXT NOT NULL, manifest TEXT NOT NULL, PRIMARY KEY (stage, id));
    CREATE TABLE IF NOT EXISTS latest_stage (stage TEXT PRIMARY KEY, id TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS build_tags (name TEXT PRIMARY KEY, build_id TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS build_search (build_id TEXT PRIMARY KEY, settings TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS tag_search (name TEXT PRIMARY KEY, settings TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS build_config (ref TEXT PRIMARY KEY, config TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS adoption_history (id INTEGER PRIMARY KEY, previous TEXT NOT NULL, adopted TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS serving_generations (id INTEGER PRIMARY KEY, build_id TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS tag_history (id INTEGER PRIMARY KEY, name TEXT NOT NULL, previous TEXT, target TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS build_runs (id TEXT PRIMARY KEY, build_id TEXT NOT NULL, config_id TEXT NOT NULL, created TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS index_outputs (key TEXT PRIMARY KEY, build_id TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS source_checks (id INTEGER PRIMARY KEY, source_id TEXT NOT NULL, checked_at TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS transform_outputs (key TEXT PRIMARY KEY, output_id TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS embeddings (id TEXT PRIMARY KEY, data BLOB NOT NULL);`);
}

async function initialize(type, directory, values) {
  const { source, purpose } = values;
  const root = resolve(directory);
  const paths = projectPaths(root);
  const configPath = join(root, configName);
  const legacyPath = join(root, 'dunlin.json');
  if (await exists(configPath) || await exists(legacyPath) || await exists(paths.data)) {
    throw new ProjectError(2, 'PROJECT_EXISTS', `A project already exists at ${root}`);
  }
  const location = source?.match(/^~[\\/]/u) ? resolve(homedir(), source.slice(2)) : source;
  const path = source === undefined ? './docs' : relative(root, resolve(location)).split(sep).join('/') || '.';
  const { ocr, ...models } = await initialSettings(type, values);
  const config = isSearchProject(type) ? ragTemplate(type, path, models) : wikiTemplate(type, basename(root), purpose, models.llm);
  if (ocr) {
    const loader = (config.source ?? config.ingress).modules.find(({ use }) => use === 'loader.auto');
    loader.with.ocr = 'loader.ocr';
    const { name, version } = toolsCatalog.get(ocr);
    config.tools = [{ name, version }];
  }
  await mkdir(root, { recursive: true });
  await writeFile(configPath, writeTemplate(config), { flag: 'wx' });
  if (isSearchProject(type) && source === undefined) await mkdir(join(root, 'docs'), { recursive: true });
  if (!isSearchProject(type)) {
    await mkdir(join(root, 'site'), { recursive: true });
    await writeFile(join(root, 'rules.md'), '# 運用の決まり\n', { flag: 'wx' }).catch((error) => {
      if (error.code !== 'EEXIST') throw error;
    });
  }
  if (type === 'para') {
    for (const category of categories) await mkdir(join(root, 'site', category), { recursive: true });
  }
  await mkdir(paths.artifacts, { recursive: true });
  const db = openState(paths);
  try {
    db.exec(`PRAGMA journal_mode = WAL;
      CREATE TABLE builds (id TEXT PRIMARY KEY, manifest TEXT NOT NULL);
      CREATE TABLE project (id INTEGER PRIMARY KEY CHECK (id = 1), build_id TEXT REFERENCES builds(id));
      INSERT INTO project VALUES (1, NULL);`);
    ensureStages(db);
    ensureEvaluation(db);
    if (!isSearchProject(type)) ensureWiki(db);
  } finally {
    db.close();
  }
  await writeFile(join(root, '.gitignore'), '.dunlin/\n.env\n.env.*\n', { flag: 'wx' }).catch((error) => {
    if (error.code !== 'EEXIST') throw error;
  });
  let next;
  if (type === 'mcp') next = 'Add documents, then run dunlin build and dunlin serve --print-config.';
  else if (type === 'rag') next = 'Add documents and run dunlin build. ' + (models.llm?.model
    ? 'Run dunlin ask or dunlin serve.' : 'Set models.answerer.model before using ask or the responses endpoint.');
  else {
    const missing = [!models.llm?.model && 'models.writer.model', !purpose && 'about.purpose'].filter(Boolean);
    next = missing.length ? 'Set ' + missing.join(' and ') + ', then run dunlin ingress.' : 'Run dunlin ingress <path>.';
  }
  return { config: configPath, type, next: ocr ? next + ' To prepare ' + ocr + ', run dunlin add ocr.' : next };
}

function normalizeProjectType(config, warnings) {
  if (config?.init?.type !== 'rag' || config.ask !== undefined || config.serve?.transport !== 'stdio'
    || Object.keys(config.serve).length !== 1) return config;
  warnings?.push({ code: 'LEGACY_PROJECT_TYPE', message: 'This legacy stdio search project runs as MCP. Set init.type to mcp to migrate it.' });
  return { ...config, init: { ...config.init, type: 'mcp' } };
}

async function loadProject(configPath, command, candidate, warnings) {
  if (!configPath || !await exists(configPath)) {
    throw new ProjectError(3, 'PROJECT_REQUIRED', 'No Dunlinfile.yaml found. Run dunlin init <mcp|rag|wiki|para> <directory>.');
  }
  const root = dirname(configPath);
  const file = candidate === undefined ? configPath : resolve(candidate);
  let parsed;
  let input;
  try {
    input = await readFile(file, 'utf8');
    parsed = parseConfig(input);
  } catch {
    throw new ProjectError(2, 'INVALID_CONFIG', 'Could not read a valid Dunlinfile.yaml');
  }
  if (parsed.syntaxError) {
    const error = new ProjectError(2, 'INVALID_CONFIG', parsed.syntaxError.message);
    error.diagnostics = [{ code: 'YAML_SYNTAX', file, line: parsed.syntaxError.line, section: null,
      module: null, arg: null, received: null, expected: 'Valid YAML', suggestions: [], hint: parsed.syntaxError.message }];
    throw error;
  }
  const { line } = parsed;
  const config = normalizeProjectType(parsed.config, warnings);
  const type = config?.init?.type;
  const authoring = ['ingress', 'query', 'verify', 'archive', 'undo', 'issue', 'rules', 'cite', 'instruct', 'save-answer', 'progress'].includes(command);
  if (!projectTypes.includes(type)) {
    const error = new ProjectError(2, 'SECTION_MISSING', 'Expected init.type in the Dunlinfile. Generate a new standard file with dunlin init.');
    error.diagnostics = [{ code: error.code, file, line: line(['init', 'type']), section: 'init', module: null,
      arg: 'type', received: null, expected: projectTypes, suggestions: [], hint: error.message }];
    throw error;
  }
  const active = candidate === undefined ? config : normalizeProjectType(parseConfig(await readFile(configPath, 'utf8')).config);
  if (candidate !== undefined) {
    if (active?.init?.type !== type) throw new ProjectError(2, 'WRONG_PROJECT_TYPE', 'Candidate and project must use the same init.type.');
    checkTrialLimits(active, () => { throw new ProjectError(2, 'INVALID_CONFIG', 'Fix trial_limits in the project Dunlinfile.'); });
  }
  if (command === 'ask' && type !== 'rag') throw new ProjectError(2, 'WRONG_PROJECT_TYPE', 'ask requires a RAG project.');
  if (!isSearchProject(type) && !authoring && !['serve', 'validate', 'defaults', 'add', 'remove'].includes(command)) {
    throw new ProjectError(2, 'WRONG_PROJECT_TYPE', 'This command requires a RAG or MCP project.');
  }
  if (isSearchProject(type) && authoring) throw new ProjectError(2, 'WRONG_PROJECT_TYPE', 'This command requires a Wiki or PARA project.');
  const effective = candidate !== undefined && command === 'eval'
    ? { ...config, ...Object.fromEntries(['eval-data', 'eval'].map((key) => [key, active[key]])) } : config;
  if (effective !== config && ['eval-data', 'eval'].some((key) => config[key] !== undefined && JSON.stringify(config[key]) !== JSON.stringify(active[key]))) {
    warnings.push({ code: 'EVAL_CONDITIONS_PINNED', message: 'This evaluation uses the dataset and method from the project Dunlinfile.' });
  }
  const checked = isSearchProject(type) ? validateRag(effective, file, line) : validateWiki(effective, file, line);
  warnings.push(...(checked.warnings ?? []));
  if (checked.errors.length && command !== 'defaults') {
    const error = new ProjectError(2, 'INVALID_CONFIG', `${checked.errors.length} configuration error(s).`);
    error.diagnostics = checked.errors;
    throw error;
  }
  const paths = projectPaths(root);
  if (!await exists(paths.state)) {
    throw new ProjectError(3, 'STATE_REQUIRED', 'Project state is missing. Initialize a project with dunlin init ' + type + '.');
  }
  const hannoyModule = isSearchProject(type) && ['build', 'index', 'eval', 'search', 'ask', 'serve', 'validate'].includes(command)
    && ['index', 'search'].flatMap((stage) => checked.plan[stage] ?? []).find(({ module }) => module.tool === 'hannoy');
  if (hannoyModule) {
    try { await checkIndexTool(paths, hannoy); }
    catch (failure) {
      const { module } = hannoyModule;
      const rows = effective[module.stage].modules;
      const error = new ProjectError(2, 'REQUIREMENT_MISSING', failure.message);
      error.diagnostics = [{ code: 'REQUIREMENT_MISSING', file, line: line([module.stage, 'modules', rows.findIndex(({ use }) => use === module.name), 'use']),
        section: module.stage, module: module.name, arg: null, received: null, expected: 'Installed hannoy ' + hannoy.version,
        suggestions: [], hint: failure.message }];
      throw error;
    }
  }
  return { root, paths, configPath, configRoot: dirname(file), configDigest: revision(input),
    plan: checked.plan, wikiPlan: isSearchProject(type) ? null : checked.plan, evalSettings: checked.evalSettings,
    serveSettings: checked.serveSettings, config: effective,
    trialLimits: trialLimits(active?.trial_limits, config.trial_limits) };
}

function currentBuild(paths, required = true, reference) {
  const db = openState(paths, true);
  try {
    const row = reference === undefined
      ? db.prepare('SELECT manifest FROM project JOIN builds ON builds.id = project.build_id').get()
      : db.prepare(`SELECT manifest FROM builds WHERE id = ? OR id = (SELECT build_id FROM build_tags WHERE name = ?)
        ORDER BY id = ? DESC LIMIT 1`).get(reference, reference, reference);
    if (row) {
      const manifest = JSON.parse(row.manifest);
      if (reference !== undefined) {
        const tag = db.prepare('SELECT settings FROM tag_search WHERE name = ?').get(reference);
        const stored = tag ?? db.prepare('SELECT settings FROM build_search WHERE build_id = ?').get(manifest.build_id);
        if (stored) manifest.search = JSON.parse(stored.settings);
        const configuration = db.prepare('SELECT config FROM build_config WHERE ref = ?').get(tag ? 'tag:' + reference : manifest.build_id);
        if (configuration) manifest.search_models = JSON.parse(configuration.config).models;
      }
      return manifest;
    }
  } finally {
    db.close();
  }
  if (required) throw new ProjectError(3, 'BUILD_REQUIRED', 'No searchable build. Run dunlin build first.');
  return null;
}

function digest(kind, value) {
  return kind + '-' + revision(JSON.stringify(value));
}

function readStage(db, stage, id) {
  const row = id === undefined
    ? db.prepare('SELECT manifest FROM stage_outputs JOIN latest_stage USING (stage, id) WHERE stage = ?').get(stage)
    : db.prepare('SELECT manifest FROM stage_outputs WHERE stage = ? AND id = ?').get(stage, id);
  if (!row) return null;
  const output = JSON.parse(row.manifest);
  if (output.payloads !== 1) return output;
  const find = db.prepare('SELECT data FROM stage_payloads WHERE id = ?');
  const get = (key) => {
    const row = find.get(key);
    if (!row) throw new ProjectError(3, 'STAGE_REQUIRED', 'A saved stage payload is missing.');
    return JSON.parse(row.data);
  };
  if (stage === 'source') output.groups = output.groups.map(([source, docs]) => [source, docs.map(({ text, ...doc }) => ({ ...doc, content: get(text) }))]);
  if (stage === 'transform') output.chunks = output.chunks.map((group) => group.map(get));
  return output;
}

function latestStageId(db, stage) {
  return db.prepare('SELECT id FROM latest_stage WHERE stage = ?').get(stage)?.id;
}

/** The settings of the source stage. Rust scans the folders of `scans` when the stage runs. */
async function sourceSettings(project, plan, { ocr }) {
  const parsed = await runModules(plan, { scans: [], parsers: [], reject: false }, {
    scan(input, { path, exclude }, { name }) {
      return { ...input, scans: [...input.scans, { path, exclude: exclude ?? [], name }] };
    },
    load(input, { category, ...args }, { name }) {
      return { ...input, parsers: [...input.parsers, { name, category, args }], reject: input.reject || args.unsupported === 'error' };
    },
  });
  const routes = parserRoutes(parsed.parsers);
  return {
    scans: parsed.scans, reject: parsed.reject, modules: moduleSettings(plan),
    routes: routes.map(({ name, category, match, ocr: reads }) => ({ name, category, ...(match && { match }), ocr: reads })),
    commands: routes.filter(({ category }) => category === 'command').map(({ name, args }) => ({ module: name, args })),
    ...(ocr && await ocrRequest(projectOcr(project.config.tools))),
  };
}

function dictionarySnapshot(paths, hash) {
  return join(paths.artifacts, 'dictionaries', hash + '.csv');
}

/** The configuration saved with a build: secrets removed and paths relative to the project root. */
function buildConfiguration(project) {
  const configuration = safeConfig(project.config);
  if (project.configRoot !== project.root) {
    for (const [stage, name, arg] of [['source', 'source.filesystem', 'path'], ['index', 'index.keyword', 'user_dictionary']]) {
      for (const entry of configuration[stage].modules) {
        const path = entry.use === name && entry.with?.[arg];
        if (!path) continue;
        if (path.includes('${') && !isAbsolute(path)) {
          const expanded = resolveConfigValue(path, name + '.with.' + arg);
          if (!isAbsolute(expanded)) entry.with[arg] = relative(project.root, project.configRoot).split(sep).join('/') + '/' + path;
        } else entry.with[arg] = relative(project.root, resolve(project.configRoot, path)).split(sep).join('/') || '.';
      }
    }
  }
  return configuration;
}

/** The settings of the index stage. Resolves the embedding model and the saved configuration when `resolve` is set. */
async function indexSettings(project, { resolve: resolved }) {
  const { keyword, vector } = await runModules(project.plan.index, {}, {
    index: (input, args) => ({ ...input, keyword: args }),
    indexVector: (input, settings, backend = 'exact') => ({ ...input, vector: { backend, settings } }),
  });
  const settings = { keyword, vector, construction: moduleSettings(project.plan.index) };
  if (!resolved) return settings;
  return { ...settings, embedder: vector ? embeddingSettings(project.config, vector.settings.model) : null,
    search: moduleSettings(project.plan.search),
    rerank: project.plan.search.filter(({ module }) => module.role === 'rerank').map(({ args }) => modelSetting(project, null, args.model)),
    configuration: buildConfiguration(project) };
}

/** Rejects with the error a host call threw, or the stage error Rust reported. */
function stageFailure({ code, message, exitCode, data }, refused) {
  if (refused) return refused;
  if (exitCode) return new ProjectError(exitCode, code, message, data ?? null);
  return Object.assign(new Error(message), code && { code });
}

async function adoptBuild(project, reference) {
  const build = currentBuild(project.paths, true, reference);
  const db = openState(project.paths);
  try {
    const byTag = db.prepare('SELECT config FROM build_config WHERE ref = ?').get('tag:' + reference);
    const stored = byTag ?? db.prepare('SELECT config FROM build_config WHERE ref = ?').get(build.build_id);
    if (!stored) throw new ProjectError(3, 'CONFIG_REQUIRED', 'This build has no saved configuration. Build the candidate again.');
    const config = JSON.parse(stored.config);
    for (const section of ['init', 'serve', 'ask', 'eval-data', 'eval', 'trial_limits']) {
      if (Object.hasOwn(project.config, section)) config[section] = project.config[section];
      else delete config[section];
    }
    const answerer = project.plan.ask?.[0]?.args.model;
    if (answerer !== undefined) config.models = { ...config.models, [answerer]: project.config.models[answerer] };
    const adopted = validateRag(config, project.configPath, () => null);
    if (adopted.errors.length) throw new ProjectError(2, 'INVALID_CONFIG', 'The adopted settings cannot be resolved. Run validate for the candidate.');
    const before = await readFile(project.configPath, 'utf8');
    if (revision(before) !== project.configDigest) throw new ProjectError(4, 'CONFIG_CHANGED', 'The configuration changed. Retry use.');
    return await withWriteLock(project.paths.data, async () => {
      db.exec('BEGIN IMMEDIATE');
      let replaced = false;
      try {
        if (await readFile(project.configPath, 'utf8') !== before) throw new ProjectError(4, 'CONFIG_CHANGED', 'The configuration changed. Retry use.');
        await atomicWrite(project.configPath, writeTemplate(config));
        replaced = true;
        const adoption = db.prepare('INSERT INTO adoption_history (previous, adopted) VALUES (?, ?)')
          .run(JSON.stringify(safeConfig(project.config)), JSON.stringify(safeConfig(config)));
        db.prepare('UPDATE project SET build_id = ? WHERE id = 1').run(build.build_id);
        db.prepare('INSERT INTO serving_generations VALUES (?, ?)').run(adoption.lastInsertRowid, build.build_id);
        const searchId = searchConfigId(adopted.plan.search, {}, build, { config });
        const evaluation = evaluationStatus(db, build.build_id, searchId, { config, evalSettings: adopted.evalSettings });
        db.exec('COMMIT');
        return { build_id: build.build_id, search_config_id: searchId, evaluation_status: evaluation.state, restart_mcp: true };
      } catch (error) {
        db.exec('ROLLBACK');
        if (replaced) await atomicWrite(project.configPath, before);
        throw error;
      }
    });
  } finally {
    db.close();
  }
}

async function tagBuild(project, name, ref, move) {
  const db = openState(project.paths);
  try { ensureStages(db); } finally { db.close(); }
  let refused;
  const result = await Dunlin.tagBuild({ state: project.paths.state, name, reference: ref, move: Boolean(move),
    search: moduleSettings(project.plan.search), configSearch: project.config.search, configModels: project.config.models }, {
    searchConfigId(build) {
      try {
        return searchConfigId(restoreModules(build.search), {}, build, project);
      } catch (error) {
        refused = error;
        throw error;
      }
    },
  });
  if (result.status === 'failed') throw stageFailure(result.error, refused);
  return result.output;
}

async function directoryBytes(root) {
  let bytes = 0;
  const dirs = [root];
  for (const dir of dirs) {
    const entries = await readdir(dir, { withFileTypes: true }).catch((error) => {
      if (error.code === 'ENOENT') return [];
      throw error;
    });
    for (const entry of entries) {
      const path = join(dir, entry.name);
      if (entry.isDirectory()) dirs.push(path);
      if (entry.isFile()) bytes += (await lstat(path).catch((error) => { if (error.code !== 'ENOENT') throw error; }))?.size ?? 0;
    }
  }
  return bytes;
}

/** A source stage made before the data layout change of step 2e (spec 28.7): moved, or not moved yet. */
function earlierLayout(source) {
  return Boolean(source && (source.migrated || source.groups?.some(([, docs]) => docs.some((document) => !('version' in document)))));
}

async function statusDetails(state, project, build, evaluation, warnings) {
  const { paths } = project;
  const snapshot = readStage(state, 'source', build.source_id);
  const previous = evaluation.report && state.prepare('SELECT manifest FROM builds WHERE id = ?').get(evaluation.report.build_id);
  const assessed = previous && readStage(state, 'source', JSON.parse(previous.manifest).source_id);
  if (assessed && earlierLayout(assessed) && !earlierLayout(snapshot)) {
    warnings.push({ code: 'REPORT_BEFORE_LAYOUT_CHANGE',
      message: 'The latest evaluation report measured a build made before the data layout change. Run eval again to compare new builds.' });
  }
  const versions = (source) => new Map((source?.groups ?? []).flatMap(([sourceId, docs]) => docs.map((document) =>
    [sourceId + ':' + document.objectId, document.sha256 ?? document.corpus?.originalSha256 ?? document.version ?? revision(document.content ?? '')])));
  const activeDocs = versions(snapshot);
  const oldDocs = versions(assessed);
  const updated = assessed ? [...activeDocs].filter(([name, hash]) => oldDocs.get(name) !== hash).length
    + [...oldDocs.keys()].filter((name) => !activeDocs.has(name)).length : null;
  const hasChecks = state.prepare("SELECT name FROM sqlite_master WHERE name = 'source_checks'").get();
  const last = hasChecks && state.prepare('SELECT checked_at FROM source_checks WHERE source_id = ? ORDER BY id DESC LIMIT 1').get(build.source_id);
  const [index, originals, corpus, metadata] = await Promise.all([
    directoryBytes(join(paths.artifacts, build.index)), directoryBytes(join(paths.artifacts, 'originals')), directoryBytes(paths.corpus), lstat(paths.state),
  ]);
  const previousConfig = state.prepare('SELECT config FROM build_config WHERE ref = ?').get(build.build_id);
  const config = previousConfig ? JSON.parse(previousConfig.config) : {};
  return { documents_changed_since_evaluation: updated, source_checked_at: last?.checked_at ?? null,
    changed_config_sections: [...stages, 'search'].filter((section) => JSON.stringify(safeConfig(project.config[section])) !== JSON.stringify(config[section])),
    storage: { build_index_logical_bytes: index, shared_originals_logical_bytes: originals,
      shared_corpus_logical_bytes: corpus, shared_state_logical_bytes: metadata.size } };
}

/** Runs a build command in Rust (spec 28.9 steps 2d and 2e). Rust also scans and orders the sources. */
async function buildProject(project, command, runId, values, warnings) {
  const selected = command === 'build'
    ? stages.slice(values.from === undefined ? 0 : stages.indexOf(values.from), values.until === undefined ? stages.length : stages.indexOf(values.until) + 1)
    : [command];
  const dryRun = Boolean(values['dry-run']);
  const { paths, plan } = project;
  const start = performance.now();
  const run = async (signal) => {
    const semantic = plan.transform.find(({ module }) => module.name === 'chunk.semantic');
    const request = {
      command, runId: runId ?? undefined, from: values.from, until: values.until, input: values.input, tag: values.tag,
      candidate: Boolean(values.config), force: Boolean(values.force), skipUnsupported: Boolean(values['skip-unsupported']), dryRun,
      state: paths.state, artifacts: paths.artifacts, corpus: paths.corpus, root: project.root,
      source: await sourceSettings(project, plan.source, { ocr: !dryRun && selected[0] === 'source' }),
      transform: { modules: moduleSettings(plan.transform),
        embedder: semantic && !dryRun && selected.includes('transform') ? embeddingSettings(project.config, semantic.args.model) : null },
      index: await indexSettings(project, { resolve: !dryRun && selected.includes('index') }),
      configIndex: project.config.index,
    };
    request.elapsedMs = performance.now() - start;
    let refused;
    const answer = (call) => async (...args) => {
      try {
        return await call(...args);
      } catch (error) {
        refused = error;
        throw error;
      }
    };
    const host = {
      ...parserHost(),
      charge: answer((bytes) => { chargeTrialApi({ byteLength: bytes }); }),
      configChanged: answer(async () => buildConfigChanged(project, await readFile(project.configPath, 'utf8'))),
      indexTool: answer(() => indexToolPath(paths, hannoy)),
    };
    let result;
    try {
      result = await Dunlin.runBuild(request, host, { signal: trialSignal(signal) });
    } catch (error) {
      trialCheckpoint();
      throw error;
    }
    if (result.status === 'failed') throw stageFailure(result.error, refused);
    warnings.push(...result.warnings);
    return result.output;
  };
  if (dryRun) return run();
  return withExecutionLock(paths, async () => {
    const db = openState(paths);
    try {
      ensureStages(db);
      return values.tag || values.config ? await runTrial(db, runId, command, project.trialLimits, run) : await run();
    } finally {
      db.close();
    }
  });
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

function buildConfigChanged(project, input) {
  if (revision(input) === project.configDigest) return false;
  const config = normalizeProjectType(parseConfig(input).config);
  if (!config || typeof config !== 'object') return true;
  const built = ['init', ...stages, 'search', 'models', 'tools'];
  return built.some((key) => JSON.stringify(config[key]) !== JSON.stringify(project.config[key]));
}

async function withExecutionLock(paths, run) {
  let lock;
  try { lock = Dunlin.acquireExecutionLock(join(paths.data, 'execution.lock')); }
  catch (error) {
    if (error.code === 'BUSY') throw new ProjectError(4, 'BUILD_BUSY', 'Another project operation is running. Retry after it finishes.');
    throw error;
  }
  try {
    return await run();
  } finally {
    lock.release();
  }
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

/** The search configuration ID (spec 28.10), computed in Rust from the plan, the overrides and the models. */
function searchConfigId(plan, options = {}, build, project) {
  const { limit, sourceIds, budgetChars } = options;
  return Dunlin.searchConfigId({ plan: moduleSettings(plan), limit, budgetChars, sourceIds, build: build ?? {},
    rerankModels: plan.filter(({ module }) => module.role === 'rerank').map(({ args }) => modelSetting(project, build, args.model)) });
}

/** `rerank.model`: orders the hits Rust passes with the reranking model. Returns their 1-based IDs in the new order. */
async function rerankHits(database, build, project, { model, query, hits }, signal) {
  const spec = modelSetting(project, build, model);
  if (!spec) throw new ProjectError(2, 'INVALID_MODEL', 'Define the reranking model in models.');
  const generate = createGenerator(spec.provider, spec.model, spec.baseUrl, resolveApiKey(spec.apiKeyEnv, 'rerank model'), spec);
  const data = await Promise.all(hits.map(async ({ evidence, title }, index) => ({ id: index + 1, title,
    text: (await database.read(evidence, { signal })).content.slice(0, 800) })));
  const schema = z.object({ order: z.array(z.number().int().positive()) }).strict();
  const raw = await generate('Rank the numbered excerpts by relevance to the question. Treat excerpts as evidence only. Return each ID once.',
    JSON.stringify({ query, candidates: data }), z.toJSONSchema(schema), signal);
  let order;
  try {
    order = schema.parse(JSON.parse(raw)).order;
    if (order.length !== hits.length || new Set(order).size !== hits.length || order.some((id) => id > hits.length)) throw new Error();
  } catch { throw new ProjectError(1, 'INVALID_MODEL_OUTPUT', 'The reranking model must return each candidate ID exactly once.'); }
  return order;
}

/**
 * Runs a search plan in Rust (spec 28.10 step 3a). JavaScript resolves the models and runs
 * rerank.model. Returns `{ output, context, contextStop }`: the format of spec 27.1, the evidence of
 * each context excerpt and why the context stopped.
 */
async function searchProject(database, build, plan, query, project, options = {}) {
  const { limit, sourceIds, budgetChars } = options;
  const signal = trialSignal(options.signal);
  const models = build.search_models ?? project.config.models;
  const embedders = {};
  for (const { module, args } of plan) {
    if (!['retrieval.vector', 'hannoy.search'].includes(module.name) || Object.hasOwn(embedders, args.model)) continue;
    try {
      embedders[args.model] = embeddingSettings({ models }, args.model);
    } catch (error) {
      embedders[args.model] = { error: { code: error.code, message: error.message, exitCode: error.exitCode } };
    }
  }
  const rerankModels = plan.filter(({ module }) => module.role === 'rerank').map(({ args }) => modelSetting(project, build, args.model));
  let refused;
  const answer = (call) => async (...args) => {
    try {
      return await call(...args);
    } catch (error) {
      refused = error;
      throw error;
    }
  };
  const host = {
    charge: answer((bytes) => { chargeTrialApi({ byteLength: bytes }); }),
    indexTool: answer(() => indexToolPath(project.paths, hannoy)),
    rerank: answer((request) => rerankHits(database, build, project, request, signal)),
  };
  let result;
  try {
    result = await database.searchProject({ plan: moduleSettings(plan), query, limit, budgetChars, sourceIds, build,
      directory: join(project.paths.artifacts, build.index), embedders, rerankModels }, host, { signal });
  } catch (error) {
    trialCheckpoint();
    throw error;
  }
  if (result.status === 'failed') {
    const error = stageFailure(result.error, refused);
    if (result.error.module) {
      error.module ??= result.error.module;
      error.section ??= 'search';
    }
    throw error;
  }
  trialCheckpoint();
  return result;
}

async function projectSearchJson(database, build, plan, query, project, options = {}) {
  return (await searchProject(database, build, plan, query, project, options)).output;
}

function keywordOptions(args, paths) {
  if (args?.tokenizer === 'lindera') {
    return { keywordJapanese: args.dictionary_hash ? { userDictionary: dictionarySnapshot(paths, args.dictionary_hash) } : {} };
  }
  if (!args || args.tokenizer === 'word') return {};
  return { keywordNgrams: true, keywordBigrams: args.short_query === 'bigram' };
}

async function openBuild(paths, build) {
  const index = join(paths.artifacts, build.index);
  if (!await Dunlin.inspectIndex(index)) {
    throw new ProjectError(3, 'BUILD_REQUIRED', 'The build index is missing. Run dunlin build first.');
  }
  return Dunlin.open(index, { corpusBackend: 'sqlite', corpusDirectory: build.shared_corpus ? paths.corpus : undefined,
    ...keywordOptions(build.keyword, paths) });
}

async function waitForHttp(server, type, host) {
  const close = () => server.close();
  process.once('SIGINT', close);
  process.once('SIGTERM', close);
  try {
    console.error('Dunlin ' + type + ': ' + serveAddress(host, server.address().port) + '/');
    await once(server, 'close');
  } finally {
    process.off('SIGINT', close);
    process.off('SIGTERM', close);
  }
}

async function serveProject(project, settings) {
  const type = project.config.init.type;
  const state = openState(project.paths, true);
  const indexes = new Map();
  let handle;
  async function closeIndex({ opening }) {
    const database = await opening.catch(() => null);
    if (!database) return;
    await database.close();
  }
  try {
    const initial = state.prepare(`SELECT manifest, coalesce((SELECT max(id) FROM adoption_history), 0) AS generation
      FROM project JOIN builds ON builds.id = project.build_id WHERE project.id = 1`).get();
    if (!initial) throw new ProjectError(3, 'BUILD_REQUIRED', 'No searchable build. Run dunlin build first.');
    const build = JSON.parse(initial.manifest);
    assertSearchIndexes(build, project.plan.search);
    const model = settings.endpoints?.includes('responses') ? answerModel(project, { optional: true }) : null;
    const database = await openBuild(project.paths, build);
    indexes.set(build.index, { opening: Promise.resolve(database), users: 0 });
    const usesKeyword = project.plan.search.some(({ module }) => module.name === 'retrieval.keyword');
    const usesVector = project.plan.search.some(({ module }) => ['retrieval.vector', 'hannoy.search'].includes(module.name));
    const activeBuild = state.prepare(`SELECT manifest FROM serving_generations JOIN builds ON builds.id = serving_generations.build_id
      WHERE serving_generations.id = ?`);
    let compatible = build;
    function closeUnusedIndex(index) {
      const entry = indexes.get(index);
      if (entry?.users === 0 && index !== compatible.index) {
        indexes.delete(index);
        return closeIndex(entry);
      }
    }
    async function withActive(callback) {
      const row = activeBuild.get(initial.generation);
      const current = row ? JSON.parse(row.manifest) : build;
      const keywordChanged = usesKeyword && JSON.stringify(current.keyword) !== JSON.stringify(build.keyword);
      const vectorChanged = usesVector && (current.vector?.identity !== build.vector?.identity
        || (current.vector?.backend ?? 'exact') !== (build.vector?.backend ?? 'exact'));
      const previousIndex = compatible.index;
      if (!keywordChanged && !vectorChanged) compatible = current;
      const selected = compatible;
      let entry = indexes.get(selected.index);
      if (!entry) {
        const opening = openBuild(project.paths, selected).catch((error) => {
          indexes.delete(selected.index);
          throw error;
        });
        entry = { opening, users: 0 };
        indexes.set(selected.index, entry);
      }
      // Concurrent requests share the open and keep their index until the callback finishes.
      entry.users++;
      try {
        return await callback(await entry.opening, selected);
      } finally {
        entry.users--;
        if (previousIndex !== selected.index || selected.index !== compatible.index) {
          await Promise.all([closeUnusedIndex(previousIndex), closeUnusedIndex(selected.index)]);
        }
      }
    }
    const search = (query, options) => withActive((db, active) =>
      projectSearchJson(db, active, project.plan.search, query, project, options));
    if (settings.transport === 'http') {
      handle = settings.endpoints.includes('mcp') ? await database.createMcpHttpHandler(search, settings.limits.max_request_bytes) : null;
      const server = await serveRagHttp(settings, { search, model: model?.model, mcp: handle,
        answer: (query, signal) => withActive(async (db, active) => {
          const result = await projectSearchJson(db, active, project.plan.search, query, project, { signal });
          return answerQuestion(project, result, query, model, signal);
        }),
      });
      await waitForHttp(server, type, settings.host);
    } else {
      const ended = new Promise((resolve) => process.stdin.once('end', resolve));
      handle = await database.serveMcp({
        transport: 'stdio', search, searchOnly: type === 'rag',
        read: async (evidence, options) => {
          try {
            return await withActive((db) => db.read(evidence, options));
          } catch (error) {
            if (!['STALE_EVIDENCE', 'NOT_FOUND'].includes(error.code)) throw error;
            const state = openState(project.paths, true);
            let builds;
            try { builds = state.prepare('SELECT manifest FROM builds ORDER BY rowid DESC').all(); }
            finally { state.close(); }
            for (const { manifest } of builds) {
              const candidate = JSON.parse(manifest);
              const archived = await openBuild(project.paths, candidate).catch(() => null);
              if (!archived) continue;
              try {
                return await archived.read(evidence, options);
              } catch (failure) {
                if (!['STALE_EVIDENCE', 'NOT_FOUND'].includes(failure.code)) throw failure;
              } finally {
                await archived.close();
              }
            }
            throw error;
          }
        },
      });
      console.error('Dunlin MCP stdio (' + build.build_id + ')');
      await ended;
    }
  } finally {
    try { await handle?.close(); }
    finally {
      state.close();
      await Promise.all([...indexes.values()].map(closeIndex));
    }
  }
}

function envelope(command, status, runId, data, warnings, errors = []) {
  return { schema_version: 1, command, status, run_id: runId, data, warnings, errors };
}

async function compareDefaults(project, section) {
  const { config, root, configPath, configDigest, paths } = project;
  const type = config.init.type;
  const source = config.source?.modules?.find?.((entry) => entry?.use === 'source.filesystem');
  const standard = isSearchProject(type) ? ragTemplate(type, source?.with?.path ?? './docs') : wikiTemplate(type, basename(root), config.about?.purpose);
  const applicable = isSearchProject(type) ? [...stages, 'search', ...(type === 'rag' ? ['ask'] : []), 'serve']
    : ['ingress', 'query', 'verify', 'archive', 'serve'];
  const differences = applicable.filter((key) => JSON.stringify(config[key]) !== JSON.stringify(standard[key]))
    .map((key) => ({ section: key, current: safeConfig(config[key]), proposed: standard[key] }));
  if (section === undefined) return { differences };
  if (!applicable.includes(section)) throw new ProjectError(2, 'INVALID_ARGUMENT', 'Choose one standard module section: ' + applicable.join(', '));
  const next = { ...config, [section]: standard[section] };
  const checked = isSearchProject(type) ? validateRag(next, configPath, () => null) : validateWiki(next, configPath, () => null);
  if (checked.errors.length) {
    const error = new ProjectError(2, 'INVALID_CONFIG', 'The resulting configuration would still be invalid.');
    error.diagnostics = checked.errors;
    throw error;
  }
  await withWriteLock(paths.data, async () => {
    if (revision(await readFile(configPath, 'utf8')) !== configDigest) throw new ProjectError(4, 'CONFIG_CHANGED', 'The Dunlinfile changed. Retry defaults.');
    await atomicWrite(configPath, writeTemplate(next));
  });
  return { applied: section, differences: differences.filter((change) => change.section === section) };
}

export async function runProjectCommand(command, args, explicitProject) {
  let json = args.slice(0, args.includes('--') ? args.indexOf('--') : args.length).includes('--json');
  let runId = null;
  const warnings = [];
  try {
    const options = { help: { type: 'boolean', short: 'h' } };
    options.json = { type: 'boolean' };
    if (command === 'init') {
      for (const key of ['source', 'purpose', 'embedding-model', 'embedding-provider', 'llm-model', 'llm-provider', 'ocr-tool']) options[key] = { type: 'string' };
      for (const key of ['no-embedding', 'no-llm', 'ocr', 'no-ocr']) options[key] = { type: 'boolean' };
    } else if (command === 'config') {
      options.provider = { type: 'string' };
      options.model = { type: 'string' };
      options.tool = { type: 'string' };
    } else options.project = { type: 'string' };
    if (command === 'build' || command === 'source') options['skip-unsupported'] = { type: 'boolean' };
    if (command === 'build' || stages.includes(command)) options['dry-run'] = { type: 'boolean' };
    if (command === 'build') {
      options.from = { type: 'string' };
      options.until = { type: 'string' };
      options.force = { type: 'boolean' };
      options.tag = { type: 'string' };
      options.input = { type: 'string' };
    }
    if (['validate', 'search', 'ask', 'build', 'eval', 'dictionary', ...stages].includes(command)) options.config = { type: 'string' };
    if (command === 'dictionary') options.report = { type: 'string' };
    if (['search', 'ask', 'eval', 'evidence-map'].includes(command)) options.build = { type: 'string' };
    if (command === 'search') {
      options.limit = { type: 'string' };
      options['budget-chars'] = { type: 'string' };
      options['source-id'] = { type: 'string', multiple: true };
    }
    if (command === 'evidence-map') {
      for (const name of ['output', 'key', 'start', 'end', 'reviewer']) options[name] = { type: 'string' };
    }
    if (['eval', 'report'].includes(command)) options.split = { type: 'string' };
    if (command === 'eval') options.tag = { type: 'string' };
    if (command === 'eval-data') {
      for (const name of ['question', 'doc', 'quote', 'prefix', 'suffix', 'test-ratio', 'seed', 'output',
        'model', 'case', 'state', 'reviewer', 'reviewer-type', 'from-search', 'hit', 'page', 'heading']) options[name] = { type: 'string' };
    }
    if (command === 'serve') {
      options['print-config'] = { type: 'boolean' };
      for (const name of ['transport', 'host', 'port']) options[name] = { type: 'string' };
    }
    if (command === 'ingress') {
      options['dry-run'] = { type: 'boolean' };
      options['source-only'] = { type: 'boolean' };
      options.pending = { type: 'boolean' };
    }
    if (['query', 'verify'].includes(command)) options.page = { type: 'string' };
    if (command === 'query') options.target = { type: 'string' };
    if (command === 'verify') {
      options.content = { type: 'boolean' };
      options['all-issues'] = { type: 'boolean' };
    }
    if (command === 'instruct') for (const option of ['target', 'supplement', 'by']) options[option] = { type: 'string' };
    if (command === 'instruct') options.persistent = { type: 'boolean' };
    if (command === 'archive') options.restore = { type: 'boolean' };
    if (command === 'tag') options.move = { type: 'boolean' };
    if (command === 'defaults') options.apply = { type: 'string' };
    if (command === 'add') options.list = { type: 'boolean' };
    const { values, positionals } = parseArgs({ args, options, allowPositionals: true });
    json = Boolean(values.json);
    if (values.help) {
      console.log(projectCommands[command]);
      return;
    }
    if (command === 'serve' && json && !values['print-config']) {
      throw new ProjectError(2, 'INVALID_ARGUMENT', 'Use --json with serve --print-config.');
    }
    const actions = command === 'config' ? { list: 1, set: 2, unset: 2 }
      : command === 'eval-data' ? { add: 1, import: 2, export: 2, list: 1, show: 2, split: 1,
      generate: 1, review: 2, sync: 2, diff: 3, tag: 3, use: 2 }
      : command === 'report' ? { show: 2, list: 1, compare: 3 }
      : command === 'dictionary' ? { suggest: 1, add: 2 }
      : command === 'evidence-map' ? { create: 1, list: 1, show: 2, export: 2, import: 2, confirm: 2 } : null;
    const count = actions ? (Object.hasOwn(actions, positionals[0]) ? actions[positionals[0]] : -1)
      : command === 'tag' ? (positionals.length === 1 ? 1 : 2)
      : ['init', 'cite', 'issue'].includes(command) || command === 'instruct' && !values.persistent
        || (command === 'modules' && positionals[0] === 'describe') ? 2
      : command === 'save-answer' ? 3
      : command === 'ingress' ? (values.pending ? 0 : positionals.length || -1)
      : ['search', 'ask', 'modules', 'use', 'query', 'archive', 'undo', 'remove', 'instruct', 'progress'].includes(command) || command === 'add' && !values.list ? 1 : 0;
    if (positionals.length !== count || positionals.some((item) => !item.trim())
      || values.source === '' || values.project === '' || (explicitProject !== undefined && values.project !== undefined)
      || (['init', 'config'].includes(command) && explicitProject !== undefined)) {
      throw new ProjectError(2, 'INVALID_ARGUMENT', projectCommands[command]);
    }
    const [type, directory] = positionals;
    if ((command === 'modules' && !['list', 'describe'].includes(type))
      || (values.tag !== undefined && (!/^[\p{L}\p{N}_.-]{1,100}$/u.test(values.tag) || (values.until && values.until !== 'index')))
      || (values.split !== undefined && !['dev', 'test'].includes(values.split))
      || [values.config, values.build, values.report].some((value) => value !== undefined && !value.trim())
      || command === 'dictionary' && (type === 'add' ? !values.config || values.report !== undefined : values.config !== undefined)
      || command === 'tag' && !/^[\p{L}\p{N}_.-]{1,100}$/u.test(type)) {
      throw new ProjectError(2, 'INVALID_ARGUMENT', projectCommands[command]);
    }
    if (command === 'init' && (!projectTypes.includes(type) || (values.source !== undefined && !isSearchProject(type))
      || (values.purpose !== undefined && (isSearchProject(type) || !values.purpose.trim())))) {
      throw new ProjectError(2, 'INVALID_ARGUMENT', projectCommands.init);
    }
    const searchOptions = { sourceIds: values['source-id'] };
    for (const [flag, key, max] of [['limit', 'limit', 100], ['budget-chars', 'budgetChars', 1000000]]) {
      const value = values[flag];
      if (value === undefined) continue;
      if (!/^\d+$/u.test(value) || Number(value) < 1 || Number(value) > max) {
        throw new ProjectError(2, 'INVALID_ARGUMENT', 'Use --' + flag + ' with an integer from 1 to ' + max + '.');
      }
      searchOptions[key] = Number(value);
    }
    if (searchOptions.sourceIds && (searchOptions.sourceIds.length > 100
      || searchOptions.sourceIds.some((id) => !/^[A-Za-z0-9_.-]{1,64}$/u.test(id)))) {
      throw new ProjectError(2, 'INVALID_ARGUMENT', 'Provide at most 100 valid source IDs.');
    }
    if ((values.input !== undefined && (!values.input.trim() || !['transform', 'index'].includes(values.from)))
      || [values.from, values.until].some((stage) => stage !== undefined && !stages.includes(stage))
      || (values.from !== undefined && values.until !== undefined && stages.indexOf(values.from) > stages.indexOf(values.until))) {
      throw new ProjectError(2, 'INVALID_ARGUMENT', 'Expected source, transform, or index in execution order for --from and --until.');
    }
    let data;
    if (command === 'modules') {
      data = describeModules(directory);
      if (!data) throw new ProjectError(2, 'MODULE_UNKNOWN', 'Unknown module: ' + directory);
      const path = await findProject(values.project ?? explicitProject);
      const active = path && parseConfig(await readFile(path, 'utf8')).config?.tools;
      if (Array.isArray(data)) data = data.filter(({ name, tool }) => {
        if (name === 'loader.ocr') return Boolean(projectOcr(active));
        return !tool || active?.some?.((entry) => entry?.name === tool);
      });
    } else if (command === 'add' && values.list) {
      data = listedTools();
    } else if (command === 'config') {
      if (type !== 'list') runId = `run-${randomUUID()}`;
      data = await configureGlobal(type, directory, values);
    } else if (command === 'init') {
      ({ DatabaseSync } = await import('node:sqlite'));
      runId = `run-${randomUUID()}`;
      data = await initialize(type, directory, values);
    } else {
      const project = await loadProject(await findProject(values.project ?? explicitProject), command, values.config, warnings);
      let serving;
      if (command === 'serve') {
        const issues = [];
        serving = withServeOverrides(project.config.init.type, project.serveSettings, values, (code, path, expected, hint) =>
          issues.push({ code, file: project.configPath, line: null, section: 'serve', module: null, arg: path.at(-1),
            received: null, expected, suggestions: [], hint }));
        if (issues.length) {
          const error = new ProjectError(2, 'INVALID_CONFIG', issues[0].hint);
          error.diagnostics = issues;
          throw error;
        }
        if (!values['print-config'] && serving.transport === 'http') serveCredential(serving);
      }
      if (command !== 'validate') ({ DatabaseSync } = await import('node:sqlite'));
      if (command === 'defaults') {
        if (values.apply !== undefined) runId = `run-${randomUUID()}`;
        data = await compareDefaults(project, values.apply);
      } else if (command === 'add' || command === 'remove') {
        runId = `run-${randomUUID()}`;
        data = await withExecutionLock(project.paths, () => changeTool(project, type, command === 'remove', writeTemplate));
      } else if (project.wikiPlan) {
        if (command === 'validate') data = { valid: true, type: project.config.init.type };
        else {
          const readOnly = Boolean(values['dry-run']) || ['cite', 'rules', 'progress'].includes(command) || command === 'verify' && !values.content;
          const db = openState(project.paths, readOnly);
          if (!readOnly) ensureWiki(db);
          const execute = (action, input, args, id, progress) => runWiki(db, project, action, input, args, id, project.paths, {
            atomicWrite, warnings, progress,
            open: (path, keyword) => Dunlin.open(path, { corpusBackend: 'sqlite', ...keywordOptions(keyword) }),
            chunk: (document) => chunkCorpus({ groups: [['docs', [document]]], unicode: 'nfkc' }, { size: 800, overlap: 100 })[0][0],
          });
          try {
            if (command === 'serve') {
              if (values['print-config']) throw new ProjectError(2, 'WRONG_PROJECT_TYPE', 'MCP launch settings are available for RAG/MCP projects.');
              const server = await serveWiki(db, project, (action, input, options = {}, id, progress) =>
                ['cite', 'rules', 'progress'].includes(action) || action === 'verify' && !options.content
                  ? execute(action, input, options, id, progress)
                  : withExecutionLock(project.paths, () => execute(action, input, options, id, progress)), serving);
              await waitForHttp(server, project.config.init.type, serving.host);
              await server.drainWork();
              return;
            }
            if (!readOnly) runId = `run-${randomUUID()}`;
            const input = values.persistent ? [null, ...positionals] : positionals;
            data = readOnly
              ? await execute(command, input, values, runId)
              : await trackWork(db, runId, command, values.page, (progress) =>
                withExecutionLock(project.paths, () => execute(command, input, values, runId, progress)), ({ run_id, stage, completed, total }) => {
                  if (['ingress', 'query', 'instruct', 'verify'].includes(command)) console.error(
                    `${run_id} ${stage}${total === undefined ? '' : ' ' + completed + '/' + total}`);
                });
          } finally {
            db.close();
          }
        }
      } else if (command === 'build' || stages.includes(command)) {
        if (!values['dry-run']) runId = `run-${randomUUID()}`;
        data = await buildProject(project, command, runId, values, warnings);
      } else if (command === 'validate') {
        data = { valid: true, type: project.config.init.type,
          stages: Object.fromEntries(Object.entries(project.plan).map(([stage, plan]) => [stage, moduleSettings(plan)])) };
      } else if (command === 'use') {
        runId = `run-${randomUUID()}`;
        data = await adoptBuild(project, positionals[0]);
      } else if (command === 'tag') {
        runId = `run-${randomUUID()}`;
        data = await tagBuild(project, type, directory, values.move);
      } else if (command === 'dictionary') {
        if (type === 'add') {
          runId = `run-${randomUUID()}`;
          data = await withExecutionLock(project.paths, () => addDictionary(project, resolve(values.config), directory));
        } else {
          const db = openState(project.paths, true);
          let database;
          try {
            const { report, data: dataset } = dictionaryEvaluation(db, project, values.report);
            const build = currentBuild(project.paths, true, report.build_id);
            if (build.keyword?.tokenizer !== 'lindera') {
              throw new ProjectError(2, 'LINDERA_REQUIRED', 'Choose an evaluation report for a Lindera build.');
            }
            database = await openBuild(project.paths, build);
            data = await suggestDictionary(report, dictionaryMisses(db, report, dataset, database), keywordOptions(build.keyword, project.paths).keywordJapanese.userDictionary);
          } finally {
            await database?.close();
            db.close();
          }
        }
      } else if (command === 'evidence-map') {
        const mutates = ['create', 'import', 'confirm'].includes(type);
        const db = openState(project.paths, !mutates);
        let database;
        try {
          const build = mutates ? currentBuild(project.paths, true, values.build) : null;
          if (build) {
            runId = `run-${randomUUID()}`;
            database = await openBuild(project.paths, build);
          }
          data = await evidenceMaps(db, project, type, directory, values, database, build);
        } finally {
          await database?.close();
          db.close();
        }
      } else if (command === 'eval-data' || command === 'eval' || command === 'report') {
        const readOnly = ['show', 'list', 'export', 'diff'].includes(type) || (type === 'review' && !values.case && !values.state);
        const run = async () => {
          const db = openState(project.paths, command === 'report' || (command === 'eval-data' && readOnly));
          try {
            if (command === 'report') return reports(db, type, positionals.slice(1), values.split);
            if (command === 'eval-data') {
              if (!readOnly) {
                ensureEvaluation(db);
                runId = `run-${randomUUID()}`;
              }
              return await manageEvaluationData(db, project, type, positionals.slice(1), values,
                () => openBuild(project.paths, currentBuild(project.paths)));
            }
            if (!project.evalSettings) throw new ProjectError(3, 'EVAL_DATA_REQUIRED', 'Add evaluation data before running eval.');
            const build = currentBuild(project.paths, true, values.build);
            const latest = latestStageId(db, 'source');
            if (latest && latest !== build.source_id) warnings.push({ code: 'OLDER_SOURCE_SNAPSHOT',
              message: 'The selected build uses an earlier saved source snapshot.' });
            const plan = build.search && !values.config ? restoreModules(build.search) : project.plan.search;
            const top = Math.max(...plan.filter(({ module }) => module.role === 'retrieval').map(({ args }) => args.top_k));
            if (project.evalSettings.recall.at.some((at) => typeof at === 'number' && at > top)) {
              throw new ProjectError(2, 'ARG_CONFLICT', 'Evaluation uses k greater than search top_k.');
            }
            const stored = db.prepare('SELECT config FROM build_config WHERE ref = ?').get(build.build_id);
            const config = stored ? JSON.parse(stored.config) : {};
            const settings = { ...project.evalSettings, contextBudget: plan.find(({ module }) => module.role === 'context').args.budget_chars,
              construction: Object.fromEntries(stages.map((stage) => [stage, build.construction?.[stage] ?? config[stage]])), searchModules: moduleSettings(plan) };
            runId = `run-${randomUUID()}`;
            const score = async () => {
              const database = await openBuild(project.paths, build);
              try {
                return await evaluate(db, project, build, settings, values, runId, database,
                  (query) => searchProject(database, values.config ? { ...build, search_models: undefined } : build, plan, query, project)
                    .then(({ output, context, contextStop }) => ({ search_config_id: output.search_config_id,
                      hits: output.results.map(({ evidence }) => ({ evidence })), context: context.map((evidence) => ({ evidence })),
                      context_chars: output.context.chars, context_stop: contextStop })));
              } finally { await database.close(); }
            };
            return values.build || values.config ? await runTrial(db, runId, command, project.trialLimits, score) : await score();
          } finally {
            db.close();
          }
        };
        data = command === 'eval' || command === 'eval-data' && type === 'generate' ? await withExecutionLock(project.paths, run) : await run();
      } else if (command === 'status') {
        const build = currentBuild(project.paths, false);
        const state = openState(project.paths, true);
        try {
          const searchId = searchConfigId(project.plan.search, {}, build, project);
          const evaluation = build && evaluationStatus(state, build.build_id, searchId, project);
          const trials = { limits: project.trialLimits, usage: trialUsage(state) };
          data = build ? { build_id: build.build_id, search_config_id: searchId,
            evaluation_status: evaluation.state, evaluation_report: evaluation.report, documents: build.documents, skipped: build.skipped,
            trials, ...await statusDetails(state, project, build, evaluation, warnings) } : { build_id: null, trials };
        } finally {
          state.close();
        }
      } else if (command === 'serve' && values['print-config']) {
        if (serving.transport === 'http') {
          if (!serving.endpoints.includes('mcp') || serving.port === 0) {
            throw new ProjectError(2, 'INVALID_ARGUMENT', 'MCP registration requires an enabled MCP endpoint and a fixed HTTP port.');
          }
          data = { url: serveAddress(serving.host, serving.port) + '/mcp' };
        } else {
          data = { command: process.execPath, args: [fileURLToPath(new URL('./cli.mjs', import.meta.url)), 'serve', '--project', project.root,
            ...(project.config.init.type === 'rag' ? ['--transport', 'stdio'] : [])] };
        }
      } else {
        if (command === 'serve') {
          for (const { message } of warnings) console.error(message);
          await serveProject(project, serving);
          return;
        }
        const build = currentBuild(project.paths, true, values.build);
        const database = await openBuild(project.paths, build);
        try {
          const plan = build.search && !values.config ? restoreModules(build.search) : project.plan.search;
          const model = command === 'ask' ? answerModel(project) : null;
          const result = await projectSearchJson(database, values.config ? { ...build, search_models: undefined } : build,
            plan, type, project, searchOptions);
          data = command === 'ask' ? await answerQuestion(project, result, type, model) : result;
        } finally {
          await database.close();
        }
      }
    }
    if (!json) for (const { message } of warnings) console.error(message);
    if (json) console.log(JSON.stringify(envelope(command, 'succeeded', runId, data, warnings)));
    else if (command === 'init') console.log(data.config + (data.next ? '\n' + data.next : ''));
    else if (command === 'config') console.log([data.path, ...['embedding', 'llm'].map((role) => {
      const setting = data[role];
      return role + '  ' + (setting ? setting.provider + ' / ' + setting.model : 'unset');
    }), 'ocr  ' + (data.ocr?.tool ?? 'unset')].join('\n'));
    else if (command === 'search') console.log(`${data.build_id} ${data.search_config_id}\n${data.context.text || 'No matches.'}`);
    else if (command === 'ask') console.log(answerText(data));
    else if (command === 'dictionary') console.log(type === 'suggest' ? dictionaryText(data) : data.user_dictionary);
    else if (command === 'status' && data.build_id) {
      console.log(`${data.build_id} ${data.search_config_id} ${data.evaluation_status}${data.evaluation_report ? ' ' + JSON.stringify(data.evaluation_report.scores) : ''}\n${JSON.stringify(data, null, 2)}`);
    }
    else if (command === 'build' && data.build_id) {
      console.log(`${data.build_id} ${data.config_id}\nDocuments: ${data.documents}/${data.files}; skipped: ${data.skipped.length}; index ${data.reused ? 'reused' : 'built'} (${data.elapsed_ms} ms).\nServing changed: ${data.serving_changed}\nNext: dunlin search "手順"`);
      for (const file of data.skipped) console.log(`${file.source_id}:${file.path} — ${file.reason}`);
      for (const [stage, result] of Object.entries(data.stages)) console.log(`${stage}: ${result.reused ? 'reused' : 'executed'} (${result.id})`);
    } else console.log(JSON.stringify(data, null, 2));
  } catch (error) {
    const argument = error.code?.startsWith('ERR_PARSE_ARGS') || error.code === 'INVALID_ARGUMENT';
    const busy = error.code === 'BUSY' || (error.code === 'ERR_SQLITE_ERROR' && (error.errcode & 255) === 5)
      || error.message?.startsWith('Another writer is updating ');
    const exitCode = error.exitCode ?? (argument ? 2 : busy ? 4 : 1);
    const code = error.exitCode ? error.code : argument ? 'INVALID_ARGUMENT' : busy ? 'DB_BUSY' : 'EXECUTION_FAILED';
    process.exitCode = exitCode;
    if (json) {
      console.log(JSON.stringify(envelope(command, 'failed', runId, error.data ?? null, warnings, error.diagnostics ?? [{
        code, message: error.message, ...(error.module && { module: error.module, section: error.section }),
      }])));
    } else {
      console.error(error.message);
      for (const item of error.diagnostics ?? []) console.error(`${item.file}:${item.line ?? '?'} ${item.section} ${item.module ?? ''} ${item.arg ?? ''} — ${item.code}: ${item.hint}${item.suggestions.length ? ' (' + item.suggestions.join(', ') + ')' : ''}`);
      for (const file of error.data?.skipped ?? []) console.error(`${file.source_id}:${file.path} — ${file.reason}`);
    }
  }
}
