// The JavaScript configuration validation that the Rust validator replaced, copied for tests.
import { stringify } from 'yaml';
import { z } from 'zod';
import { resolve, dirname } from 'node:path';

import { KnowledgeNest } from 'knowledge-nest';

import { resolveConfigValue } from '../cli-reference/config.mjs';
import { checkTools } from './project-tools.mjs';
import { checkTrialLimits, trialCheckpoint } from './project-trials.mjs';
import { isSearchProject, serveTemplate, validateServe } from './project-serving.mjs';
import { projectModel } from '../cli-reference/project-global.mjs';
import { checkCommand } from './project-parsers.mjs';

// The module registry (names, roles, argument definitions and descriptions) is in Rust (spec 26.5).
// JavaScript keeps only how each module runs, until execution moves to Rust (spec 28.6).
const runners = {
  'source.filesystem': (input, args, ctx) => ctx.scan(input, args, ctx),
  'loader.auto': (input, args, ctx) => ctx.load(input, { ...args, category: 'auto' }, ctx),
  'loader.text': (input, args, ctx) => ctx.load(input, { ...args, category: 'text' }, ctx),
  'loader.office': (input, args, ctx) => ctx.load(input, { ...args, category: 'office' }, ctx),
  'loader.pdf': (input, args, ctx) => ctx.load(input, { ...args, category: 'pdf' }, ctx),
  'loader.ocr': (input, args, ctx) => ctx.load(input, { ...args, ocr: 'loader.ocr', category: 'ocr' }, ctx),
  'loader.command': (input, args, ctx) => ctx.load(input, { ...args, category: 'command' }, ctx),
  'text.normalize': (input, args) => ({ ...input, unicode: args.unicode }),
  'chunk.length': (input, args, ctx) => ctx.chunk(input, args),
  'chunk.structure': (input, args, ctx) => ctx.chunk(input, { size: args.max_chars, overlap: 0, structure: true }),
  'chunk.semantic': (input, args, ctx) => ctx.chunkSemantic(input, args),
  'index.keyword': (input, args, ctx) => ctx.index(input, args),
  'index.vector': (input, args, ctx) => ctx.indexVector(input, args),
  'hannoy.index': (input, args, ctx) => ctx.indexVector(input, args, 'hannoy'),
  'query.normalize': (input, args) => ({ ...input,
    query: args.unicode === 'nfkc' ? input.query.trim().normalize('NFKC') : input.query.trim() }),
  'retrieval.keyword': (input, args, ctx) => ctx.retrieve(input, args),
  'retrieval.vector': (input, args, ctx) => ctx.retrieveVector(input, args),
  'hannoy.search': (input, args, ctx) => ctx.retrieveVector(input, args, 'hannoy'),
  'fusion.rrf': (input, args, ctx) => ctx.fuse(input, args),
  'rerank.model': (input, args, ctx) => ctx.rerank(input, args),
  'context.assemble': (input, args, ctx) => ctx.assemble(input, args),
  'answer.generate': (input, args, ctx) => ctx.answer(input, args),
};
const descriptions = KnowledgeNest.describeModules();
const modules = new Map(descriptions.map((info) => [info.name, { ...info, run: runners[info.name] }]));

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
  const values = {};
  for (const { key, value, code } of KnowledgeNest.readModuleArgs(name, overrides)) {
    if (code) throw new Error(`Invalid default ${key} for ${name}`);
    if (value !== undefined) values[key] = value;
  }
  return { use: name, with: values };
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
      source: 'Additional parser: loader.ocr (requires nest add ocr).',
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

/** Reads KnowledgeNestfile.yaml in Rust. `line(path)` gives the line of the longest existing prefix of path. */
export function parseConfig(text) {
  const parsed = KnowledgeNest.parseConfig(text);
  if (parsed.syntaxError) return { syntaxError: parsed.syntaxError };
  // Array positions and keys share one form, so ['modules', 0] and ['modules', '0'] find the same line.
  const key = (path) => JSON.stringify(path.map(String));
  const lines = new Map(parsed.lines.map(([path, line]) => [key(path), line]));
  const line = (path) => {
    for (let length = path.length; length > 0; length--) {
      const found = lines.get(key(path.slice(0, length)));
      if (found !== undefined) return found;
    }
    return null;
  };
  return { config: parsed.config, line };
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

export function describeModules(name) {
  if (name === undefined) return structuredClone(descriptions);
  const info = descriptions.find((item) => item.name === name);
  return info && structuredClone(info);
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
      issue('ARG_UNKNOWN', [key], 'A supported top-level section', 'See the KnowledgeNestfile example in section 5.5.', { received: key });
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
  if (config.init.version !== 1) issue('VERSION_UNSUPPORTED', ['init', 'version'], 1, 'Generate a version 1 configuration with nest init.', { received: config.init.version });
  for (const [stage, names] of configuredStages) {
    const section = config[stage];
    if (section === undefined) {
      issue('SECTION_MISSING', [stage], stage, 'Generate the standard section with nest init ' + config.init.type + ' <directory>.');
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
        issue('MODULE_UNKNOWN', [...path, 'use'], 'A registered module name', 'Run nest modules list.', {
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
          'Use an argument listed by nest modules describe ' + name + '.', { ...details, received: key, suggestions: nearby(key, Object.keys(module.args)) });
      }
      const values = {};
      for (const { key, value, defaulted, code, expected } of KnowledgeNest.readModuleArgs(name, args)) {
        const argPath = [...path, 'with', key];
        if (code) {
          issue(code, argPath, expected, 'Use a value allowed by the module argument definition.', { ...details, received: args[key] });
        } else {
          values[key] = value;
          if (value !== undefined && defaulted) diagnostic(warnings, 'ARG_DEFAULT', argPath, expected,
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
        issue('REQUIREMENT_MISSING', [...path, 'use'], 'The ocr tool', 'Run nest add ocr.', details);
      }
      if (name === 'loader.command' && values.command) {
        const { issues, warning } = checkCommand(values, file);
        for (const { field, expected, hint } of issues) issue('REQUIREMENT_MISSING', [...path, 'with', ...field], expected, hint, details);
        if (warning) diagnostic(warnings, 'ARG_RECOMMENDED', [...path, 'with', ...warning.field], warning.expected, warning.hint, details);
      }
      if (module.tool && !config.tools?.some?.((tool) => tool?.name === module.tool)) {
        issue('REQUIREMENT_MISSING', [...path, 'use'], 'The ' + module.tool + ' tool', 'Run nest add ' + module.tool + '.', details);
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
    issue('ARG_TYPE', ['eval-data'], 'A mapping with a dataset reference in use', 'Select data with nest eval-data use.');
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
