// The zod argument definitions and descriptions that the Rust module registry replaced.
// Tests compare the registry with them. Run functions are kept only to preserve the shape.
import { z } from 'zod';

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
// chunk.context came after the JavaScript implementation; the reference follows the Rust one.
register('chunk.context', 'transform', 'context', 'chunks', 'chunks', {
  headings: z.boolean().default(true), document: z.boolean().default(true), model: z.string().min(1).optional(),
  document_chars: integer(100, 1000000, 8000), max_chars: integer(20, 4000, 300),
}, "Give each chunk its place in its document, which the index searches before the chunk's text: the document's name, the headings above the chunk (Markdown headings, or numbered and marked lines of other text), and optionally a short description a chat model writes from the document. Search results keep the chunk's own range and text.",
(input) => input);
register('index.keyword', 'index', 'index', 'chunks', 'index', {
  tokenizer: choice(['lindera', 'trigram', 'word'], 'lindera'),
  user_dictionary: z.string().trim().min(1).nullable().optional(),
  short_query: z.enum(['bigram', 'none']).optional(),
}, 'Build the BM25 keyword index.', (input, args, ctx) => ctx.index(input, args));
// The arguments below that have no default came after the zod registry; they are written here as
// the Rust registry reads them.
const prefix = z.string().min(1).optional();
const weight = z.number().min(0).max(100).optional();
register('index.vector', 'index', 'index', 'chunks', 'index', {
  model: z.string().min(1), document_prefix: prefix,
}, 'Build an exact vector index from the configured chunks.', (input, args, ctx) => ctx.indexVector(input, args),
{ external: true, deterministic: false, requires: ['embedding model'] });
register('hannoy.index', 'index', 'index', 'chunks', 'index', {
  model: z.string().min(1), ef_construction: integer(32, 10000, 100), document_prefix: prefix,
}, 'Build an incremental HNSW index using the installed hannoy tool.',
(input, args, ctx) => ctx.indexVector(input, args, 'hannoy'),
{ tool: 'hannoy', external: true, deterministic: false, requires: ['hannoy tool', 'embedding model'] });
register('query.normalize', 'search', 'preprocess', 'query', 'query', {
  unicode: choice(['nfkc', 'none'], 'nfkc'),
}, 'Normalize the query.', (input, args) => ({ ...input,
  query: args.unicode === 'nfkc' ? input.query.trim().normalize('NFKC') : input.query.trim() }));
register('retrieval.keyword', 'search', 'retrieval', 'query', 'candidates', {
  top_k: integer(1, 100, 10), weight,
}, 'Retrieve ranked keyword matches.', (input, args, ctx) => ctx.retrieve(input, args), { requires: ['index.keyword'] });
register('retrieval.vector', 'search', 'retrieval', 'query', 'candidates', {
  model: z.string().min(1), top_k: integer(1, 100, 10), weight, query_prefix: prefix,
}, 'Rank chunks by cosine similarity to the query embedding.', (input, args, ctx) => ctx.retrieveVector(input, args),
{ external: true, deterministic: false, requires: ['index.vector', 'embedding model'] });
register('hannoy.search', 'search', 'retrieval', 'query', 'candidates', {
  model: z.string().min(1), top_k: integer(1, 100, 10), ef_search: integer(1, 100000, 100), weight, query_prefix: prefix,
}, 'Retrieve candidates from the hannoy HNSW index.',
(input, args, ctx) => ctx.retrieveVector(input, args, 'hannoy'),
{ tool: 'hannoy', external: true, deterministic: false, requires: ['hannoy.index', 'embedding model'] });
register('fusion.rrf', 'search', 'fusion', 'candidates', 'candidates', {
  k: integer(1, 1000, 60),
}, 'Combine ranked candidate lists using reciprocal rank fusion, weighting each list by the weight of its retrieval module.',
(input, args, ctx) => ctx.fuse(input, args));
register('rerank.model', 'search', 'rerank', 'candidates', 'candidates', {
  model: z.string().min(1), candidates: integer(1, 32, 10),
}, 'Reorder the top candidates with a chat model while keeping their original ranges.',
(input, args, ctx) => ctx.rerank(input, args), { external: true, deterministic: false, requires: ['chat model'] });
register('rerank.cross_encoder', 'search', 'rerank', 'candidates', 'candidates', {
  url: z.string().min(1), model: z.string().min(1).optional(), format: choice(['cohere', 'tei'], 'cohere'),
  api_key_env: z.string().min(1).optional(), candidates: integer(1, 100, 20), max_chars: integer(1, 100000, 2000),
}, 'Reorder the top candidates with a reranking model behind a rerank endpoint (Cohere or Jina /v1/rerank, or TEI /rerank) while keeping their original ranges.',
(input, args, ctx) => ctx.rerank(input, args), { external: true, deterministic: false, requires: ['rerank endpoint'] });
register('context.assemble', 'search', 'context', 'candidates', 'context', {
  budget_chars: integer(1, 1000000, 4000),
  retrieval: choice(['fill_budget', 'top_k'], 'fill_budget'),
  max_candidates: integer(1, 100, 100),
  merge_overlaps: z.boolean().default(true),
  truncate_last: z.boolean().default(true),
  include_headings: z.boolean().default(true),
  expand_lines: z.number().int().min(0).max(1000).optional(),
  expand_section_chars: z.number().int().min(1).max(1000000).optional(),
}, 'Assemble source excerpts within the character budget, optionally widening each to the section it is in when the section has at most expand_section_chars characters, or else by expand_lines lines around it.',
(input, args, ctx) => ctx.assemble(input, args));
register('answer.generate', 'ask', 'answer', 'context', 'response', {
  model: z.string().min(1).default('answerer'), cite: z.boolean().default(true),
  on_no_evidence: choice(['decline'], 'decline'),
}, 'Generate an answer grounded in the supplied context.', (input, args, ctx) => ctx.answer(input, args),
{ external: true, deterministic: false, requires: ['chat model'] });


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


export { modules as referenceModules, argumentInfo as referenceArgumentInfo };
