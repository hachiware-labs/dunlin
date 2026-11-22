import assert from 'node:assert/strict';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { stringify } from 'yaml';

import { parseConfig, validateRag } from './fixtures/cli-reference/project-modules.mjs';
import { toolsCatalog } from './fixtures/cli-reference/project-tools.mjs';
import { ragTemplate, validateRag as referenceValidate } from './fixtures/reference-config/project-modules.mjs';

const embedding = { provider: 'ollama', model: 'embed' };
const llm = { provider: 'ollama', model: 'chat' };
const at = (config, stage, use) => config[stage].modules.find((entry) => entry.use === use);

// Each mutation returns the configuration to validate, starting from a fresh template.
const mutations = {
  standard: (c) => c,
  topUnknown: (c) => ({ ...c, extra: 1, sourc: {} }),
  toolsNotList: (c) => ({ ...c, tools: 'ocr' }),
  toolsBad: (c) => ({ ...c, tools: [{ name: 'ocr', version: 'old' }, { name: 'zzz' }, 'x', { name: 'hannoy', version: toolsCatalog.get('hannoy').version, extra: 1 }] }),
  toolsDuplicate: (c) => ({ ...c, tools: [{ name: 'ocr', version: toolsCatalog.get('ocr').version }, { name: 'ocr', version: toolsCatalog.get('ocr').version }] }),
  trialBad: (c) => ({ ...c, trial_limits: { max_runs: -1 } }),
  trialUnknown: (c) => ({ ...c, trial_limits: { max_runs: 2, other: 1 } }),
  trialOk: (c) => ({ ...c, trial_limits: { max_runs: 2, max_elapsed_ms: 1000 } }),
  initExtra: (c) => ({ ...c, init: { ...c.init, extra: true } }),
  versionTwo: (c) => ({ ...c, init: { ...c.init, version: 2 } }),
  versionMissing: (c) => ({ ...c, init: { type: c.init.type } }),
  modelsNotMap: (c) => ({ ...c, models: ['x'] }),
  modelsBad: (c) => ({ ...c, models: { a: { kind: 'x', provider: 1, model: 5, base_url: 3, api_key_env: '1x', max_input_bytes: 0, zz: 1 }, b: 'str', c: null,
    d: { kind: 'chat', max_input_bytes: 1.5 }, e: { kind: 'chat', base_url: 'https://u:p@host/x?q=1#f' }, f: { kind: 'chat', base_url: 'ftp://host' }, g: { kind: 'chat', base_url: 'not a url' } } }),
  sectionMissing: (c) => { const next = { ...c }; delete next.transform; return next; },
  sectionNotMap: (c) => ({ ...c, index: ['x'] }),
  modulesNotList: (c) => ({ ...c, index: { modules: {} } }),
  sectionExtraKey: (c) => ({ ...c, index: { ...c.index, size: 1 } }),
  entryBad: (c) => { c.transform.modules.unshift('x', { use: 3 }); return c; },
  moduleUnknown: (c) => { c.transform.modules[1].use = 'chunk.lenght'; return c; },
  moduleWrongStage: (c) => { c.transform.modules.push({ use: 'index.keyword' }); return c; },
  orderMismatch: (c) => { c.transform.modules.reverse(); return c; },
  roleDuplicate: (c) => { c.transform.modules.push({ use: 'chunk.structure', with: { max_chars: 10 } }); return c; },
  aliasConflict: (c) => { c.source.modules.push({ use: 'loader.auto' }, { use: 'loader.text', as: '' }, { use: 'loader.pdf', as: 'x' }, { use: 'loader.office', as: 'x' }, { use: 'loader.ocr', as: 5 }); return c; },
  entryExtraKey: (c) => { c.index.modules[0].extra = 1; return c; },
  withNotMap: (c) => { c.index.modules[0].with = 'x'; return c; },
  argUnknown: (c) => { c.transform.modules[1].with.sise = 1; c.transform.modules[1].with.zzzzzzzzzz = 2; return c; },
  argBad: (c) => { c.transform.modules[1].with = { size: 1.5, overlap: -1, unit: 'words' }; return c; },
  argDefaulted: (c) => { c.transform.modules[1].with = {}; return c; },
  pathEnvMissing: (c) => { c.source.modules[0].with.path = '${KNOWLEDGE_NEST_TEST_MISSING_VAR}/docs'; return c; },
  pathEnvPresent: (c) => { c.source.modules[0].with.path = '${KNOWLEDGE_NEST_TEST_PRESENT_VAR}/docs'; return c; },
  pathAbsolute: (c) => { c.source.modules[0].with.path = '../shared/./docs/'; return c; },
  pathBlank: (c) => { c.source.modules[0].with.path = '${KNOWLEDGE_NEST_TEST_EMPTY_VAR}'; return c; },
  overlap: (c) => { c.transform.modules[1].with.overlap = 900; return c; },
  ocrWithoutTool: (c) => { c.source.modules.push({ use: 'loader.ocr' }); at(c, 'source', 'loader.auto').with.ocr = 'loader.ocr'; return c; },
  ocrWithTool: (c) => ({ ...mutations.ocrWithoutTool(c), tools: [{ name: 'ocr', version: toolsCatalog.get('ocr').version }] }),
  commandChecks: (c) => { c.source.modules.push({ use: 'loader.command', with: { match: ['*.x'], command: ['{input}', 'a{bogus}', '{{literal}}', '{name}{ext}', '{', 'x}'], input: 'file', version: '1', version_command: ['knowledge-nest-no-such-tool', '{input}'], env: { A: 'x\u0000', B: '${KNOWLEDGE_NEST_TEST_MISSING_VAR}' } } }); return c; },
  commandStdin: (c) => { c.source.modules.push({ use: 'loader.command', with: { match: ['*.x'], command: [process.execPath, '{input}'], input: 'stdin' } }); return c; },
  commandNoInput: (c) => { c.source.modules.push({ use: 'loader.command', with: { match: ['*.x'], command: [process.execPath, '--version'] } }); return c; },
  commandRelative: (c) => { c.source.modules.push({ use: 'loader.command', with: { match: ['*.x'], command: ['./missing-tool', '{input}'], version: '1' } }); return c; },
  hannoyWithoutTool: (c) => { c.index.modules.push({ use: 'hannoy.index', with: { model: 'embedder' } }); return c; },
  keywordTrigram: (c) => { at(c, 'index', 'index.keyword').with = { tokenizer: 'trigram' }; return c; },
  keywordBigramConflict: (c) => { at(c, 'index', 'index.keyword').with = { tokenizer: 'word', short_query: 'bigram', user_dictionary: 'dict.csv' }; return c; },
  keywordDictionary: (c) => { at(c, 'index', 'index.keyword').with = { tokenizer: 'lindera', user_dictionary: ' ${KNOWLEDGE_NEST_TEST_PRESENT_VAR}/dict.csv ' }; return c; },
  keywordDictionaryMissing: (c) => { at(c, 'index', 'index.keyword').with = { tokenizer: 'lindera', user_dictionary: '${KNOWLEDGE_NEST_TEST_MISSING_VAR}.csv' }; return c; },
  rerank: (c) => { c.search.modules.splice(-1, 0, { use: 'rerank.model', with: { model: 'missing' } }); return c; },
  rerankOk: (c) => ({ ...mutations.rerank(c), models: { ...c.models, missing: { kind: 'chat', provider: 'ollama', model: 'm' } } }),
  answerBad: (c) => { if (c.ask) c.ask.modules[0].with.model = 'nothing'; return c; },
  semantic: (c) => { c.transform.modules[1] = { use: 'chunk.semantic', with: { model: 'embedder', max_chars: 100, sentence_chars: 200 } }; return c; },
  fusionWithoutPair: (c) => { c.search.modules.splice(-1, 0, { use: 'fusion.rrf', with: { k: 60 } }); return c; },
  twoRetrievalsNoFusion: (c) => { c.search.modules.splice(-1, 0, { use: 'retrieval.vector', with: { model: 'embedder', top_k: 10 } }); return c; },
  vectorModelMissing: (c) => { c.index.modules.push({ use: 'index.vector', with: { model: 'nope' } }); return c; },
  vectorDuplicate: (c) => { c.index.modules.push({ use: 'index.vector', with: { model: 'embedder' } }, { use: 'index.vector', as: 'v2', with: { model: 'embedder' } }); return c; },
  normalizationMismatch: (c) => { c.search.modules[0].with.unicode = 'none'; return c; },
  maxCandidates: (c) => { at(c, 'search', 'context.assemble').with.max_candidates = 5; return c; },
  contextNotLast: (c) => { c.search.modules.push({ use: 'query.normalize', as: 'q2' }); return c; },
  serveMissing: (c) => { const next = { ...c }; delete next.serve; return next; },
  serveNotMap: (c) => ({ ...c, serve: 'x' }),
  serveStdioExtras: (c) => ({ ...c, serve: { transport: 'stdio', host: 'x', port: 1, other: 1 } }),
  serveTransportNull: (c) => ({ ...c, serve: { transport: null } }),
  serveBadTransport: (c) => ({ ...c, serve: { transport: 'udp' } }),
  serveHttp: (c) => ({ ...c, serve: { transport: 'http', host: 'example.com', port: 70000, endpoints: [1, 'search', 'search', 'web'], pages: '', zz: 1,
    access: { allowed_hosts: ['bad host', 'Example.COM', '[::1]', '127.0.0.1', '1.2.3'], allowed_origins: ['http://a.com', 'http://a.com/', 'ftp://x'], q: 1,
      auth: { type: 'x', token_env: '1A', w: 1 } }, limits: { max_request_bytes: 0, request_timeout_seconds: 1.5, max_concurrent: 1e20, e: 1 } } }),
  serveHttpTypes: (c) => ({ ...c, serve: { transport: 'http', host: 5, endpoints: 'x', pages: 3, access: { allowed_hosts: [], auth: 'x' }, limits: [] } }),
  serveHttpAccessNull: (c) => ({ ...c, serve: { transport: 'http', access: null } }),
  serveHostNoToken: (c) => ({ ...c, serve: { ...c.serve, host: '0.0.0.0' } }),
  serveTokenNoEnv: (c) => ({ ...c, serve: { ...c.serve, access: { auth: { type: 'token' } } } }),
  serveNoneWithEnv: (c) => ({ ...c, serve: { ...c.serve, host: '::1', access: { auth: { type: 'none', token_env: 'TOKEN' } } } }),
  servePagesForRag: (c) => ({ ...c, serve: { ...c.serve, pages: './site' } }),
  evalDataBad: (c) => ({ ...c, 'eval-data': { use: 1, x: 2 } }),
  evalNotMap: (c) => ({ ...c, eval: [] }),
  evalBad: (c) => ({ ...c, eval: { split: 'test', include_generated: 'yes', other: 1, modules: [{ use: 'metric.nope' }, 'x',
    { use: 'metric.evidence_recall', with: { at: ['ctx', 0, 101, 1.5, 'context'], short_evidence_chars: 0, min_coverage: 2, unresolved: 'x', q: 1 }, extra: 1 },
    { use: 'metric.latency', with: { percentiles: [] } }, { use: 'metric.latency', with: { percentiles: [0, 'a'], z: 1 } }] } }),
  evalGood: (c) => ({ ...c, eval: { split: 'dev', include_generated: null, modules: [
    { use: 'metric.latency', with: { percentiles: [50, 95] } },
    { use: 'metric.evidence_recall', with: { unresolved: 'count_as_missed', min_coverage: 0.8, at: ['context', 10], short_evidence_chars: 40 } }] } }),
  evalMissingMetrics: (c) => ({ ...c, eval: { split: 'dev', modules: [] } }),
  nonFinite: (c) => { c.transform.modules[1].with.size = NaN; at(c, 'search', 'context.assemble').with.budget_chars = Infinity; return c; },
  secrets: (c) => { c.source.modules[0].with.unknown_secret = 'sk-12345'; c.source.modules[0].with.path = 5; return c; },
};

const templates = {
  mcp: () => ragTemplate('mcp', './docs'),
  rag: () => ragTemplate('rag', './docs', { llm }),
  ragVector: () => ragTemplate('rag', './docs', { embedding, llm }),
};

function comparable(result) {
  return JSON.parse(JSON.stringify({
    errors: result.errors, warnings: result.warnings, serveSettings: result.serveSettings, evalSettings: result.evalSettings,
    plan: Object.fromEntries(Object.entries(result.plan).map(([stage, entries]) =>
      [stage, entries.map(({ module, args, name }) => ({ use: module.name, name, args }))])),
  }, (_key, value) => typeof value === 'number' && !Number.isFinite(value) ? String(value) : value));
}

test('validates configurations exactly as the JavaScript validation did', async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'knowledge-nest-validate-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const file = join(root, 'KnowledgeNestfile.yaml');
  process.env.KNOWLEDGE_NEST_TEST_PRESENT_VAR = 'present';
  process.env.KNOWLEDGE_NEST_TEST_EMPTY_VAR = '   ';
  delete process.env.KNOWLEDGE_NEST_TEST_MISSING_VAR;
  let compared = 0;
  for (const [templateName, template] of Object.entries(templates)) {
    for (const [mutationName, mutate] of Object.entries(mutations)) {
      const config = mutate(template());
      const text = stringify(config);
      await writeFile(file, text);
      const parsed = parseConfig(text);
      const label = templateName + ' / ' + mutationName;
      // YAML cannot hold NaN in the same way, so in-memory values are validated without lines too.
      for (const [input, line] of [[mutationName === 'nonFinite' ? config : parsed.config, parsed.line], [config, () => null]]) {
        const expected = comparable(referenceValidate(structuredClone(input), file, line));
        const actual = comparable(validateRag(structuredClone(input), file, line));
        assert.deepEqual(actual, expected, label);
        compared++;
      }
    }
  }
  assert.ok(compared > 150);
});
