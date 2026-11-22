import { stringify } from 'yaml';

import { KnowledgeNest } from 'knowledge-nest';

import { toolsCatalog } from './project-tools.mjs';
import { isSearchProject, serveTemplate } from './project-serving.mjs';
import { projectModel } from './project-global.mjs';

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
      source: 'Additional parser: loader.ocr (requires knowledge-nest add ocr).',
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
  line.entries = parsed.lines;
  return { config: parsed.config, line };
}

export function describeModules(name) {
  if (name === undefined) return structuredClone(descriptions);
  const info = descriptions.find((item) => item.name === name);
  return info && structuredClone(info);
}

/**
 * Validates an MCP or RAG configuration in Rust (spec 26.8). `line` comes from parseConfig; its
 * entries give diagnostics their lines.
 */
export function validateRag(config, file, line) {
  const tools = [...toolsCatalog.values()].map(({ name, version }) => [name, version]);
  const result = KnowledgeNest.validateConfig(config, { file, lines: line?.entries ?? [], tools });
  const plan = Object.fromEntries(Object.entries(result.plan).map(([stage, entries]) =>
    [stage, entries.map(({ use, name, args }) => ({ module: modules.get(use), args, name }))]));
  return { plan, errors: result.errors, warnings: result.warnings, evalSettings: result.evalSettings, serveSettings: result.serveSettings };
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
      output = await module.run(output, args, { ...context, name });
    } catch (error) {
      error.module ??= module.name;
      error.section ??= module.stage;
      throw error;
    }
  }
  return output;
}
