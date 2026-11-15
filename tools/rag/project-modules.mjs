import { LineCounter, parseDocument, stringify } from 'yaml';
import { z } from 'zod';
import { resolve, dirname } from 'node:path';
import { resolveConfigValue } from './config.mjs';
import { checkTools } from './project-tools.mjs';
import { checkTrialLimits, trialCheckpoint } from './project-trials.mjs';
import { isSearchProject, serveTemplate, validateServe } from './project-serving.mjs';
import { projectModel } from './project-global.mjs';
import { checkCommand } from './project-parsers.mjs';

const integer = (min, max, value) => z.number().int().min(min).max(max).default(value);
const choice = (values, value) => z.enum(values).default(value);

const modules = new Map();
function register(name, stage, role, input, output, args, description, run, options = {}) {
  modules.set(name, { name, version: 1, stage, role, input, output, args, description, run,
    deterministic: true, external: false, unit: ['parser', 'preprocess', 'chunk'].includes(role) && stage !== 'search' ? 'document' : 'collection',
    requires: [], ...options });
}

register('source.filesystem', 'source', 'source', 'project', 'files', {
  path: z.string().min(1).default('./docs'), exclude: z.array(z.string()).default([]),
}, 'Scan a local folder.', (input, args, ctx) => ctx.scan(input, args, ctx));
// Parsers are matched in the written order. loader.auto is the fallback and is matched last.
const match = z.array(z.string().min(1)).min(1).optional();
register('loader.auto', 'source', 'parser', 'files', 'corpus', {
  unsupported: choice(['error', 'skip'], 'error'), ocr: choice(['none', 'loader.ocr'], 'none'),
}, 'Route files that no other parser claimed by format, and extract the text and source positions.',
(input, args, ctx) => ctx.load(input, { ...args, category: 'auto' }, ctx));
for (const [category, description] of Object.entries({
  text: 'Read UTF-8 text, Markdown and HTML, preserving headings.',
  office: 'Read Word, Excel and PowerPoint documents as structured text.',
  pdf: 'Extract the existing text layer and page positions from PDFs.',
})) {
  register('loader.' + category, 'source', 'parser', 'files', 'corpus', {
    unsupported: choice(['error', 'skip'], 'error'), match,
  }, description, (input, args, ctx) => ctx.load(input, { ...args, category }, ctx));
}
register('loader.ocr', 'source', 'parser', 'files', 'corpus', {
  unsupported: choice(['error', 'skip'], 'error'), match,
}, 'Extract text and original page positions with the installed OCR model.',
(input, args, ctx) => ctx.load(input, { ...args, ocr: 'loader.ocr', category: 'ocr' }, ctx), { requires: ['ocr tool'] });
const argv = z.array(z.string().min(1)).min(1);
register('loader.command', 'source', 'parser', 'files', 'corpus', {
  match: z.array(z.string().min(1)).min(1),
  command: argv,
  input: choice(['file', 'stdin'], 'file'),
  output: choice(['markdown', 'text', 'json'], 'markdown'),
  version: z.string().min(1).optional(),
  version_command: argv.optional(),
  env: z.record(z.string().regex(/^[A-Za-z_][A-Za-z0-9_]*$/u), z.string()).default({}),
  timeout_ms: integer(1000, 3_600_000, 120_000),
  concurrency: integer(1, 4, 2),
}, 'Extract matching files with an external command that prints Markdown, text or JSON.',
(input, args, ctx) => ctx.load(input, { ...args, category: 'command' }, ctx),
{ external: true, deterministic: false, requires: ['external command'], speedArgs: ['timeout_ms', 'concurrency'] });
register('text.normalize', 'transform', 'preprocess', 'corpus', 'corpus', {
  unicode: choice(['nfkc', 'none'], 'nfkc'),
}, 'Normalize text while preserving original positions.', (input, args) => ({ ...input, unicode: args.unicode }));
register('chunk.length', 'transform', 'chunk', 'corpus', 'chunks', {
  unit: choice(['characters'], 'characters'), size: integer(1, 1048576, 800), overlap: integer(0, 1048575, 100),
}, 'Split into overlapping character windows.', (input, args, ctx) => ctx.chunk(input, args));
register('chunk.structure', 'transform', 'chunk', 'corpus', 'chunks', {
  max_chars: integer(1, 1048576, 1600),
}, 'Split at headings and page boundaries, with a length cap.',
(input, args, ctx) => ctx.chunk(input, { size: args.max_chars, overlap: 0, structure: true }));
register('chunk.semantic', 'transform', 'chunk', 'corpus', 'chunks', {
  model: z.string().min(1), max_chars: integer(1, 1048576, 1200), sentence_chars: integer(1, 16384, 250),
  similarity: z.number().min(-1).max(1).default(0.55),
}, 'Split text at low-similarity boundaries between adjacent passages.',
(input, args, ctx) => ctx.chunkSemantic(input, args), { external: true, deterministic: false, requires: ['embedding model'] });
register('index.keyword', 'index', 'index', 'chunks', 'index', {
  tokenizer: choice(['lindera', 'trigram', 'word'], 'lindera'),
  user_dictionary: z.string().trim().min(1).nullable().optional(),
  short_query: z.enum(['bigram', 'none']).optional(),
}, 'Build the BM25 keyword index.', (input, args, ctx) => ctx.index(input, args));
register('index.vector', 'index', 'index', 'chunks', 'index', {
  model: z.string().min(1),
}, 'Build an exact vector index from the configured chunks.', (input, args, ctx) => ctx.indexVector(input, args),
{ external: true, deterministic: false, requires: ['embedding model'] });
register('hannoy.index', 'index', 'index', 'chunks', 'index', {
  model: z.string().min(1), ef_construction: integer(32, 10000, 100),
}, 'Build an incremental HNSW index using the installed hannoy tool.',
(input, args, ctx) => ctx.indexVector(input, args, 'hannoy'),
{ tool: 'hannoy', external: true, deterministic: false, requires: ['hannoy tool', 'embedding model'] });
register('query.normalize', 'search', 'preprocess', 'query', 'query', {
  unicode: choice(['nfkc', 'none'], 'nfkc'),
}, 'Normalize the query.', (input, args) => ({ ...input,
  query: args.unicode === 'nfkc' ? input.query.trim().normalize('NFKC') : input.query.trim() }));
register('retrieval.keyword', 'search', 'retrieval', 'query', 'candidates', {
  top_k: integer(1, 100, 10),
}, 'Retrieve ranked keyword matches.', (input, args, ctx) => ctx.retrieve(input, args), { requires: ['index.keyword'] });
register('retrieval.vector', 'search', 'retrieval', 'query', 'candidates', {
  model: z.string().min(1), top_k: integer(1, 100, 10),
}, 'Rank chunks by cosine similarity to the query embedding.', (input, args, ctx) => ctx.retrieveVector(input, args),
{ external: true, deterministic: false, requires: ['index.vector', 'embedding model'] });
register('hannoy.search', 'search', 'retrieval', 'query', 'candidates', {
  model: z.string().min(1), top_k: integer(1, 100, 10), ef_search: integer(1, 100000, 100),
}, 'Retrieve candidates from the hannoy HNSW index.',
(input, args, ctx) => ctx.retrieveVector(input, args, 'hannoy'),
{ tool: 'hannoy', external: true, deterministic: false, requires: ['hannoy.index', 'embedding model'] });
register('fusion.rrf', 'search', 'fusion', 'candidates', 'candidates', {
  k: integer(1, 1000, 60),
}, 'Combine ranked candidate lists using reciprocal rank fusion.', (input, args, ctx) => ctx.fuse(input, args));
register('rerank.model', 'search', 'rerank', 'candidates', 'candidates', {
  model: z.string().min(1), candidates: integer(1, 32, 10),
}, 'Reorder the top candidates with a chat model while keeping their original ranges.',
(input, args, ctx) => ctx.rerank(input, args), { external: true, deterministic: false, requires: ['chat model'] });
register('context.assemble', 'search', 'context', 'candidates', 'context', {
  budget_chars: integer(1, 1000000, 4000),
  retrieval: choice(['fill_budget', 'top_k'], 'fill_budget'),
  max_candidates: integer(1, 100, 100),
  merge_overlaps: z.boolean().default(true),
  truncate_last: z.boolean().default(true),
  include_headings: z.boolean().default(true),
}, 'Assemble source excerpts within the character budget.', (input, args, ctx) => ctx.assemble(input, args));
register('answer.generate', 'ask', 'answer', 'context', 'response', {
  model: z.string().min(1).default('answerer'), cite: z.boolean().default(true),
  on_no_evidence: choice(['decline'], 'decline'),
}, 'Generate an answer grounded in the supplied context.', (input, args, ctx) => ctx.answer(input, args),
{ external: true, deterministic: false, requires: ['chat model'] });

export const constructionStages = ['source', 'transform', 'index'];
const sequences = {
  source: ['source.filesystem', 'loader.auto'],
  transform: ['text.normalize', 'chunk.length'],
  index: ['index.keyword'],
  search: ['query.normalize', 'retrieval.keyword', 'context.assemble'],
  ask: ['answer.generate'],
};
const required = {
  source: { source: 1, parser: 1 },
  transform: { chunk: 1 },
  index: { index: 1 },
  search: { retrieval: 1, context: 1 },
  ask: { answer: 1 },
};
const modelSchema = z.strictObject({
  kind: z.enum(['chat', 'embedding']), provider: z.enum(['openai', 'ollama', 'strata']).optional(),
  model: z.string().nullable().optional(), base_url: z.string().optional(),
  api_key_env: z.string().regex(/^[A-Za-z_][A-Za-z0-9_]*$/u).optional(),
  max_input_bytes: z.number().int().positive().optional(),
});
const sectionComments = {
  source: 'Scan and extract documents. Optional OCR requires a loader.ocr module.',
  transform: 'Normalize and split text. Additional chunking modules can replace chunk.length.',
  index: 'Build the keyword index. An optional vector index uses index.vector.',
  search: 'Retrieve candidates and assemble source context.',
  ask: 'Generate a sourced answer from the search context.',
  serve: 'Expose the project through its configured transport.',
};

function defaultEntry(name, overrides = {}) {
  const { args } = modules.get(name);
  return { use: name, with: z.object(args).parse(overrides) };
}

export function ragTemplate(type, path, { embedding, llm } = {}) {
  const config = { init: { version: 1, type } };
  for (const [stage, names] of Object.entries(sequences)) {
    if (stage === 'ask' && type !== 'rag') continue;
    config[stage] = { modules: names.map((name) => defaultEntry(name,
      name === 'source.filesystem' ? { path } : name === 'index.keyword' ? { user_dictionary: null } : {})) };
  }
  if (type === 'rag') config.models = { answerer: projectModel('chat', llm) };
  if (embedding) {
    const reference = { model: 'embedder' };
    config.models = { ...config.models, embedder: projectModel('embedding', embedding) };
    config.index.modules.push(defaultEntry('index.vector', reference));
    config.search.modules.splice(-1, 0, defaultEntry('retrieval.vector', reference), defaultEntry('fusion.rrf'));
  }
  config.serve = serveTemplate(type);
  return config;
}

export function wikiTemplate(type, title, purpose, llm) {
  const entry = (use, args = {}) => ({ use, with: args });
  return {
    init: { version: 1, type },
    about: { title, purpose: purpose ?? '', out_of_scope: '' },
    models: { writer: projectModel('chat', llm) },
    ingress: { modules: [
      defaultEntry('loader.auto'), defaultEntry('index.keyword', { tokenizer: 'trigram', short_query: 'bigram' }),
      entry(type === 'wiki' ? 'wiki.write' : 'para.write', { model: 'writer', rules: './rules.md', max_pages_per_job: 20 }),
      entry('citation.verify', { on_failure: 'reject' }),
    ] },
    query: { modules: [entry('wiki.search', { include_archived: false }), entry('wiki.answer', { model: 'writer' })] },
    verify: {
      modules: ['check.citations', 'check.stale_versions', 'check.links', 'check.orphans'].map((name) => entry(name)),
      content: [entry('check.contradictions', { model: 'writer', scope: 'changed_and_related' })],
    },
    archive: { modules: [entry('wiki.archive', { keep_links: true })] },
    serve: serveTemplate(type),
  };
}

export function writeTemplate(config) {
  let yaml = stringify(config);
  if (isSearchProject(config.init.type)) {
    for (const [stage, example] of Object.entries({
      source: 'Additional parser: loader.ocr (requires dunlin add ocr).',
      transform: 'Alternative chunking: use: chunk.structure, with: { max_chars: 1600 }.',
      index: 'Additional index: use: index.vector, with: { model: local-embed }.',
      search: 'For hybrid search, add retrieval.vector and fusion.rrf before context.assemble.',
    })) {
      if (stage === 'index' && config.index?.modules?.some?.(({ use }) => ['index.vector', 'hannoy.index'].includes(use))
        || stage === 'search' && config.search?.modules?.some?.(({ use }) => use === 'fusion.rrf')) continue;
      yaml = yaml.replace(new RegExp('^' + stage + ':$', 'mu'), '# ' + example + '\n' + stage + ':');
    }
  }
  return yaml.replace(/^(source|transform|index|search|ask|serve):$/gmu, (text, section) =>
    '# ' + sectionComments[section] + '\n' + text);
}

export function parseConfig(text) {
  const counter = new LineCounter();
  const yaml = parseDocument(text, { lineCounter: counter });
  if (yaml.errors.length) {
    const error = yaml.errors[0];
    return { syntaxError: { line: error.linePos?.[0]?.line ?? null, message: 'Invalid YAML syntax or duplicate key.' } };
  }
  let config;
  try { config = yaml.toJS({ maxAliasCount: 0 }); }
  catch { return { syntaxError: { line: null, message: 'YAML aliases are unsupported.' } }; }
  const line = (path) => {
    for (let length = path.length; length > 0; length--) {
      const node = yaml.getIn(path.slice(0, length), true);
      if (node?.range) return counter.linePos(node.range[0]).line;
    }
    return null;
  };
  return { config, line };
}

function isMapping(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function distance(a, b) {
  let row = Array.from({ length: b.length + 1 }, (_, i) => i);
  for (const ch of a) {
    const next = [row[0] + 1];
    for (let i = 0; i < b.length; i++) next.push(Math.min(next[i] + 1, row[i + 1] + 1, row[i] + (ch !== b[i])));
    row = next;
  }
  return row.at(-1);
}

function nearby(name, options) {
  if (typeof name !== 'string' || name.length > 120) return [];
  return options.map((option) => [option, distance(name, option)])
    .filter(([, score]) => score <= 4).sort((a, b) => a[1] - b[1]).slice(0, 3).map(([option]) => option);
}

function safeValue(value) {
  if (typeof value === 'string' && (/^\$\{[A-Z0-9_]+\}$/iu.test(value)
    || /^(?:[^\s:/]+:\/\/|sk-|Bearer |gh[pousr]_)/iu.test(value))) return '[redacted]';
  return value === null || ['string', 'number', 'boolean'].includes(typeof value) ? value : null;
}

const argumentTypes = { ZodNumber: 'integer', ZodString: 'string', ZodArray: 'array', ZodBoolean: 'boolean', ZodEnum: 'one of the listed values',
  ZodRecord: 'mapping' };
function argumentInfo(schema, effect = 'output') {
  const inner = schema instanceof z.ZodDefault || schema instanceof z.ZodOptional ? schema.unwrap() : schema;
  const fallback = schema.safeParse(undefined);
  return { type: inner instanceof z.ZodNumber && !inner.isInt ? 'number' : argumentTypes[inner.constructor.name],
    ...(fallback.success && fallback.data !== undefined && { default: fallback.data }),
    ...(inner.options && { choices: inner.options }),
    ...(inner.minValue !== undefined && { min: inner.minValue, max: inner.maxValue }),
    ...(schema instanceof z.ZodOptional && { optional: true }),
    effect };
}

export function describeModules(name) {
  const describe = ({ args, run, speedArgs = [], ...info }) => ({ ...info, args: Object.fromEntries(
    Object.entries(args).map(([key, schema]) => [key, argumentInfo(schema, speedArgs.includes(key) ? 'speed' : 'output')]),
  ) });
  if (name !== undefined) {
    const module = modules.get(name);
    return module && describe(module);
  }
  return [...modules.values()].map(describe);
}

export function validateRag(config, file, line) {
  const errors = [];
  const warnings = [];
  const plan = {};
  const diagnostic = (target, code, path, expected, hint, details = {}) => {
    const [section, , , location, arg] = path;
    target.push({ code, file, line: line(path), section: section ?? null, module: details.module ?? null,
      arg: location === 'with' ? arg ?? null : null, received: safeValue(details.received), expected,
      suggestions: details.suggestions ?? [], hint });
  };
  const error = (...args) => diagnostic(errors, ...args);
  const issue = (code, path, expected, hint, details) => error(code, path, expected, hint, details);
  const configuredStages = Object.entries(sequences).filter(([stage]) => stage !== 'ask' || config.init.type === 'rag');
  for (const key of Object.keys(config)) {
    if (!['init', ...configuredStages.map(([stage]) => stage), 'serve', 'models', 'tools', 'eval-data', 'eval', 'trial_limits'].includes(key)) {
      issue('ARG_UNKNOWN', [key], 'A supported top-level section', 'See the Dunlinfile example in section 5.5.', { received: key });
    }
  }
  checkTools(config, issue);
  checkTrialLimits(config, issue);
  for (const key of Object.keys(config.init)) {
    if (!['version', 'type'].includes(key)) issue('ARG_UNKNOWN', ['init', key], 'version or type', 'Remove the unsupported init key.', { received: key });
  }
  if (config.models !== undefined) {
    if (!isMapping(config.models)) issue('ARG_TYPE', ['models'], 'Named model definitions', 'Write each model as a mapping.');
    else for (const [name, value] of Object.entries(config.models)) {
      const model = modelSchema.safeParse(value);
      if (!model.success) {
        for (const failure of model.error.issues) issue(failure.code === 'unrecognized_keys' ? 'ARG_UNKNOWN' : 'ARG_TYPE',
          ['models', name, ...failure.path], 'A typed chat or embedding model with an environment variable for credentials',
          'Check the model definition and keep credentials in the environment.');
      }
      const url = typeof value?.base_url === 'string' && URL.parse(value.base_url);
      if (url && (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.search || url.hash)) {
        issue('ARG_RANGE', ['models', name, 'base_url'], 'A credential-free HTTP(S) endpoint', 'Use api_key_env for credentials.');
      }
    }
  }
  if (config.init.version !== 1) issue('VERSION_UNSUPPORTED', ['init', 'version'], 1, 'Generate a version 1 configuration with dunlin init.', { received: config.init.version });
  for (const [stage, names] of configuredStages) {
    const section = config[stage];
    if (section === undefined) {
      issue('SECTION_MISSING', [stage], stage, 'Generate the standard section with dunlin init ' + config.init.type + ' <directory>.');
      continue;
    }
    if (!isMapping(section) || !Array.isArray(section.modules)) {
      issue('ARG_TYPE', [stage], 'A mapping with a modules list', 'Write modules as a list of use/with entries.');
      continue;
    }
    for (const key of Object.keys(section)) {
      if (key !== 'modules') issue('ARG_UNKNOWN', [stage, key], 'modules', 'Move module arguments under with.', { received: key });
    }
    const roleCount = {};
    const used = new Set();
    const explicitNames = new Set();
    let type = modules.get(names[0]).input;
    let chainValid = true;
    plan[stage] = [];
    for (const [i, entry] of section.modules.entries()) {
      const path = [stage, 'modules', i];
      if (!isMapping(entry) || typeof entry.use !== 'string') {
        issue('ARG_TYPE', path, 'A module entry containing a string use', 'Write use: ' + names[0] + '.');
        chainValid = false;
        continue;
      }
      const name = entry.use;
      const module = modules.get(name);
      const details = { module: name };
      if (!module) {
        issue('MODULE_UNKNOWN', [...path, 'use'], 'A registered module name', 'Run dunlin modules list.', {
          ...details, received: name, suggestions: nearby(name, [...modules.keys()]),
        });
        chainValid = false;
        continue;
      }
      if (module.stage !== stage) {
        issue('MODULE_WRONG_STAGE', [...path, 'use'], module.stage, 'Move the module to ' + module.stage + '.', { ...details, received: name });
        chainValid = false;
        continue;
      }
      const parallelRole = ['source', 'parser', 'index', 'retrieval'].includes(module.role) && module.output === type
        && (module.role !== 'retrieval' || !roleCount.fusion);
      if (module.input !== type && chainValid && !parallelRole) {
        issue('TYPE_MISMATCH', [...path, 'use'], type, 'Order modules so outputs connect to the next input.', { ...details, received: module.input });
      }
      type = module.output;
      roleCount[module.role] = (roleCount[module.role] ?? 0) + 1;
      if (roleCount[module.role] > 1 && !['source', 'parser', 'index', 'retrieval', 'preprocess', 'rerank'].includes(module.role)) {
        issue('ROLE_DUPLICATE', [...path, 'use'], 'One ' + module.role + ' module', 'Remove the duplicate module.', details);
      }
      const instance = entry.as ?? name;
      if (typeof instance !== 'string' || !instance.trim() || explicitNames.has(instance) || (used.has(name) && entry.as === undefined)) {
        issue('ARG_CONFLICT', [...path, 'as'], 'A unique module name or alias', 'Set a distinct as value on repeated modules.', details);
      }
      used.add(name);
      explicitNames.add(instance);
      for (const key of Object.keys(entry)) {
        if (!['use', 'with', 'as'].includes(key)) issue('ARG_UNKNOWN', [...path, key], 'use, with, or as', 'Use with for module arguments.', { ...details, received: key });
      }
      const args = entry.with === undefined ? {} : entry.with;
      if (!isMapping(args)) {
        issue('ARG_TYPE', [...path, 'with'], 'An argument mapping', 'Write a mapping under with.', details);
        continue;
      }
      for (const key of Object.keys(args)) {
        if (!Object.hasOwn(module.args, key)) issue('ARG_UNKNOWN', [...path, 'with', key], Object.keys(module.args),
          'Use an argument listed by dunlin modules describe ' + name + '.', { ...details, received: key, suggestions: nearby(key, Object.keys(module.args)) });
      }
      const values = {};
      for (const [key, schema] of Object.entries(module.args)) {
        const checked = schema.safeParse(args[key]);
        const argPath = [...path, 'with', key];
        if (!checked.success) {
          const typeError = checked.error.issues[0].code === 'invalid_type';
          issue(args[key] === undefined ? 'ARG_MISSING' : typeError ? 'ARG_TYPE' : 'ARG_RANGE', argPath, argumentInfo(schema),
            'Use a value allowed by the module argument definition.', { ...details, received: args[key] });
        } else {
          values[key] = checked.data;
          if (checked.data !== undefined && !Object.hasOwn(args, key)) diagnostic(warnings, 'ARG_DEFAULT', argPath, argumentInfo(schema),
            'Write the default value explicitly to make the configuration reproducible.', details);
        }
      }
      if (name === 'source.filesystem' && values.path) {
        try { values.path = resolve(dirname(file), resolveConfigValue(values.path, name + '.with.path')); }
        catch (failure) { issue('REQUIREMENT_MISSING', [...path, 'with', 'path'], 'A resolvable local path', failure.message, details); }
      }
      if (name === 'chunk.length' && values.overlap >= values.size) {
        issue('ARG_CONFLICT', [...path, 'with', 'overlap'], 'overlap less than size', 'Reduce overlap or increase size.', { ...details, received: values.overlap });
      }
      if ((name === 'loader.ocr' || values.ocr === 'loader.ocr') && !config.tools?.some?.((tool) => tool?.name === 'ocr')) {
        issue('REQUIREMENT_MISSING', [...path, 'use'], 'The ocr tool', 'Run dunlin add ocr.', details);
      }
      if (name === 'loader.command' && values.command) {
        const { issues, warning } = checkCommand(values, file);
        for (const { field, expected, hint } of issues) issue('REQUIREMENT_MISSING', [...path, 'with', ...field], expected, hint, details);
        if (warning) diagnostic(warnings, 'ARG_RECOMMENDED', [...path, 'with', ...warning.field], warning.expected, warning.hint, details);
      }
      if (module.tool && !config.tools?.some?.((tool) => tool?.name === module.tool)) {
        issue('REQUIREMENT_MISSING', [...path, 'use'], 'The ' + module.tool + ' tool', 'Run dunlin add ' + module.tool + '.', details);
      }
      if (name === 'index.keyword') {
        if (values.tokenizer === 'trigram') values.short_query ??= 'bigram';
        else if (values.short_query === 'bigram') issue('ARG_CONFLICT', [...path, 'with', 'short_query'],
          'bigram with the trigram tokenizer', 'Remove short_query or select the trigram tokenizer.', details);
        if (values.user_dictionary && values.tokenizer !== 'lindera') issue('ARG_CONFLICT', [...path, 'with', 'user_dictionary'],
          'A CSV with the lindera tokenizer', 'Select lindera or remove user_dictionary.', details);
        if (values.user_dictionary) {
          try { values.user_dictionary = resolve(dirname(file), resolveConfigValue(values.user_dictionary, name + '.with.user_dictionary')); }
          catch (failure) { issue('REQUIREMENT_MISSING', [...path, 'with', 'user_dictionary'], 'A resolvable local CSV path', failure.message, details); }
        }
      }
      if (name === 'rerank.model' && values.model) {
        const model = config.models?.[values.model];
        if (model?.kind !== 'chat' || !model.model || !model.provider) issue('REQUIREMENT_MISSING', [...path, 'with', 'model'],
          'A named chat model', 'Define a chat model under models.', details);
        if (roleCount.retrieval > 1 && !roleCount.fusion) issue('ARG_CONFLICT', [...path, 'use'], 'Fusion before reranking',
          'Place fusion.rrf before rerank.model.', details);
      }
      if (name === 'answer.generate' && values.model) {
        const model = config.models?.[values.model];
        if (model?.kind !== 'chat' || !model.provider) issue('REQUIREMENT_MISSING', [...path, 'with', 'model'],
          'A named chat model', 'Define the answer model under models.', details);
      }
      if (name === 'chunk.semantic') {
        const model = values.model && config.models?.[values.model];
        if (values.model && (model?.kind !== 'embedding' || !model.model || !['ollama', 'openai'].includes(model.provider))) {
          issue('REQUIREMENT_MISSING', [...path, 'with', 'model'], 'An Ollama or OpenAI embedding model', 'Define an embedding model under models.', details);
        }
        if (values.sentence_chars > values.max_chars) issue('ARG_CONFLICT', [...path, 'with', 'sentence_chars'], 'At most max_chars',
          'Reduce sentence_chars or increase max_chars.', details);
      }
      plan[stage].push({ module, args: values, name: instance });
    }
    for (const role of Object.keys(required[stage])) {
      if (!roleCount[role]) issue('ROLE_MISSING', [stage, 'modules'], role, 'Add the required module: ' + names.find((name) => modules.get(name).role === role) + '.');
    }
    if (stage === 'search' && type !== 'context' && roleCount.context) {
      issue('TYPE_MISMATCH', [stage, 'modules'], 'Context assembly as the final module', 'Move context.assemble to the end.');
    }
  }
  const context = plan.search?.find(({ module }) => module.role === 'context')?.args;
  const retrievals = plan.search?.filter(({ module }) => module.role === 'retrieval') ?? [];
  const normalizedText = plan.transform?.findLast(({ module }) => module.name === 'text.normalize')?.args.unicode ?? 'none';
  const normalizedQuery = plan.search?.findLast(({ module }) => module.name === 'query.normalize')?.args.unicode ?? 'none';
  const fusion = plan.search?.find(({ module }) => module.role === 'fusion');
  if (Boolean(fusion) !== (retrievals.length > 1)) {
    issue('ARG_CONFLICT', ['search', 'modules'], 'fusion.rrf after two or more retrieval modules',
      retrievals.length > 1 ? 'Add fusion.rrf before context.assemble.' : 'Remove fusion.rrf or add another retrieval module.');
  }
  const index = plan.index ?? [];
  const vectors = index.filter(({ module }) => ['index.vector', 'hannoy.index'].includes(module.name));
  const vector = vectors[0];
  if (vectors.length > 1 || index.filter(({ module }) => module.name === 'index.keyword').length > 1) {
    issue('ROLE_DUPLICATE', ['index', 'modules'], 'One vector backend and one keyword index', 'Choose a vector backend for this build.');
  }
  if (vector?.args.model) {
    const model = config.models?.[vector.args.model];
    if (model?.kind !== 'embedding' || !model.model || !['ollama', 'openai'].includes(model.provider)) {
      issue('REQUIREMENT_MISSING', ['index', 'modules', index.indexOf(vector), 'with', 'model'], 'An Ollama or OpenAI embedding model',
        'Define the embedding model under models.', { module: vector.module.name });
    }
  }
  for (const { module, args } of retrievals) {
    const target = { 'retrieval.vector': 'index.vector', 'hannoy.search': 'hannoy.index' }[module.name];
    const compatible = target ? vectors.some(({ module, args: input }) => module.name === target && input.model === args.model)
      : index.some(({ module }) => module.name === 'index.keyword');
    if (!compatible) issue('REQUIREMENT_MISSING', ['search', 'modules'], module.requires, 'Define the matching index and model.', { module: module.name });
  }
  if (normalizedText !== normalizedQuery) {
    issue('REQUIREMENT_MISSING', ['search', 'modules'], normalizedText, 'Use the same Unicode normalization for text and queries.', { module: 'query.normalize' });
  }
  if (context?.retrieval === 'fill_budget' && retrievals.some(({ args }) => context.max_candidates < args.top_k)) {
    issue('ARG_CONFLICT', ['search', 'modules', plan.search.findIndex(({ module }) => module.role === 'context'), 'with', 'max_candidates'],
      'At least each retrieval top_k', 'Increase max_candidates or decrease top_k.', { module: 'context.assemble', received: context.max_candidates });
  }
  const serveSettings = validateServe(config.init.type, config.serve, issue);
  const evalSettings = validateEvaluation(config, issue);
  return { plan, errors, warnings, evalSettings, serveSettings };
}

const recallArgs = z.strictObject({
  at: z.array(z.union([z.literal('context'), z.number().int().min(1).max(100)])).min(1),
  short_evidence_chars: z.number().int().positive(), min_coverage: z.number().min(0).max(1),
  unresolved: z.literal('count_as_missed'),
});
const latencyArgs = z.strictObject({ percentiles: z.array(z.number().int().min(1).max(100)).min(1) });

function validateEvaluation(config, issue) {
  if (config['eval-data'] !== undefined && (!isMapping(config['eval-data'])
    || Object.keys(config['eval-data']).some((key) => key !== 'use') || typeof config['eval-data'].use !== 'string')) {
    issue('ARG_TYPE', ['eval-data'], 'A mapping with a dataset reference in use', 'Select data with dunlin eval-data use.');
  }
  if (config.eval === undefined) return null;
  const settings = config.eval;
  if (!isMapping(settings) || !Array.isArray(settings.modules)) {
    issue('ARG_TYPE', ['eval'], 'A split and modules list', 'See section 5.5 for the evaluation configuration.');
    return null;
  }
  for (const key of Object.keys(settings)) {
    if (!['split', 'modules', 'include_generated'].includes(key)) issue('ARG_UNKNOWN', ['eval', key], 'split, modules or include_generated', 'Remove the unknown key.', { received: key });
  }
  if (settings.split !== 'dev') issue('ARG_RANGE', ['eval', 'split'], 'dev', 'Use --split test explicitly to run the test split.');
  if (settings.include_generated !== undefined && typeof settings.include_generated !== 'boolean') {
    issue('ARG_TYPE', ['eval', 'include_generated'], 'A boolean', 'Set true or false.');
  }
  const output = { includeGenerated: settings.include_generated ?? false };
  const seen = new Set();
  for (const [i, item] of settings.modules.entries()) {
    const path = ['eval', 'modules', i];
    const name = item?.use;
    const schema = name === 'metric.evidence_recall' ? recallArgs : name === 'metric.latency' ? latencyArgs : null;
    if (!schema) { issue('MODULE_UNKNOWN', path, 'metric.evidence_recall or metric.latency', 'Use a supported evaluation module.', { received: name }); continue; }
    if (seen.has(name)) issue('ROLE_DUPLICATE', path, 'A single ' + name, 'Remove duplicates.', { module: name });
    seen.add(name);
    for (const key of Object.keys(item)) {
      if (!['use', 'with'].includes(key)) issue('ARG_UNKNOWN', [...path, key], 'use or with', 'Remove the unknown key.', { module: name, received: key });
    }
    const checked = schema.safeParse(item.with);
    if (!checked.success) {
      for (const failure of checked.error.issues) issue(failure.code === 'unrecognized_keys' ? 'ARG_UNKNOWN' : 'ARG_TYPE',
        [...path, 'with', ...failure.path], 'Valid ' + name + ' arguments', 'See section 5.5.', { module: name });
    } else if (name === 'metric.evidence_recall') output.recall = checked.data;
    else output.percentiles = checked.data.percentiles;
  }
  for (const name of ['metric.evidence_recall', 'metric.latency']) {
    if (!seen.has(name)) issue('ROLE_MISSING', ['eval', 'modules'], name, 'Add ' + name + '.');
  }
  return output;
}

export function moduleSettings(plan) {
  return plan.map(({ module, args, name }) => ({ use: module.name, version: module.version, name, with: args }));
}

export function restoreModules(settings) {
  return settings.map(({ use, name, with: args }) => ({ module: modules.get(use), name, args }));
}

export async function runModules(plan, initial, context) {
  let output = initial;
  for (const { module, args, name } of plan) {
    try {
      trialCheckpoint();
      output = await module.run(output, args, { ...context, name });
      trialCheckpoint();
    } catch (error) {
      error.module ??= module.name;
      error.section ??= module.stage;
      throw error;
    }
  }
  return output;
}
