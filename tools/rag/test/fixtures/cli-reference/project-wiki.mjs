import { z } from 'zod';

import { checkTools, projectOcr } from './project-tools.mjs';
import { validateServe } from './project-serving.mjs';

// Wiki and PARA projects run in Rust (spec 28.12): their commands and their screen. JavaScript
// checks the configuration until stage 6.

export function validateWiki(config, file, line) {
  const errors = [];
  const add = (code, path, expected, hint) => errors.push({
    code, file, line: line(path), section: path[0], module: null, arg: path.at(-1), received: null, expected, suggestions: [], hint,
  });
  const known = ['init', 'about', 'models', 'tools', 'ingress', 'query', 'verify', 'archive', 'serve'];
  for (const key of Object.keys(config)) if (!known.includes(key)) add('ARG_UNKNOWN', [key], known, 'Remove the unknown section.');
  checkTools(config, add);
  if (config.init.version !== 1) add('VERSION_UNSUPPORTED', ['init', 'version'], 1, 'Create a version 1 project.');
  const loader = z.strictObject({ unsupported: z.enum(['error', 'skip']), ocr: z.enum(['none', 'loader.ocr']) });
  const keyword = z.strictObject({ tokenizer: z.literal('trigram'), short_query: z.literal('bigram') });
  const writerArgs = z.strictObject({ model: z.string(), rules: z.string(), max_pages_per_job: z.number().int().min(1).max(32) });
  const modelRef = z.strictObject({ model: z.string() });
  const empty = z.strictObject({});
  const checks = {
    ingress: [['loader.auto', loader], ['index.keyword', keyword], [config.init.type + '.write', writerArgs],
      ['citation.verify', z.strictObject({ on_failure: z.literal('reject') })]],
    query: [['wiki.search', z.strictObject({ include_archived: z.boolean() })], ['wiki.answer', modelRef]],
    verify: ['check.citations', 'check.stale_versions', 'check.links', 'check.orphans'].map((name) => [name, empty]),
    archive: [['wiki.archive', z.strictObject({ keep_links: z.literal(true) })]],
  };
  const plan = {};
  function validateSequence(stage, key, sequence) {
    const fields = stage === 'verify' ? ['modules', 'content'] : ['modules'];
    if (key === 'modules' && config[stage] && typeof config[stage] === 'object') {
      for (const field of Object.keys(config[stage])) if (!fields.includes(field)) add('ARG_UNKNOWN', [stage, field], fields, 'Remove the unknown key.');
    }
    const list = config[stage]?.[key];
    if (!Array.isArray(list) || list.length !== sequence.length) {
      add('SECTION_MISSING', [stage, key], sequence.map(([name]) => name), 'Use the standard sequence from a newly initialized project.');
      return [];
    }
    return list.map((entry, i) => {
      const [name, schema] = sequence[i];
      const path = [stage, key, i];
      if (entry?.use !== name) add('MODULE_UNKNOWN', [...path, 'use'], name, 'Use the supported module in this position.');
      if (entry && Object.keys(entry).some((key) => !['use', 'with'].includes(key))) {
        add('ARG_UNKNOWN', path, 'use and with', 'Remove the unknown key.');
      }
      const args = schema.safeParse(entry?.with ?? {});
      if (!args.success) add('ARG_TYPE', [...path, 'with'], name + ' arguments', 'Use the standard arguments in section 13.5.');
      if (typeof entry?.with?.model === 'string' && config.models?.[entry.with.model]?.kind !== 'chat') {
        add('REQUIREMENT_MISSING', [...path, 'with', 'model'], 'A chat model reference', 'Define the model under models.');
      }
      return args.success ? args.data : null;
    });
  }
  for (const [stage, sequence] of Object.entries(checks)) plan[stage] = validateSequence(stage, 'modules', sequence);
  if (plan.ingress[0]?.ocr === 'loader.ocr' && !projectOcr(config.tools)) {
    add('REQUIREMENT_MISSING', ['ingress', 'modules', 0, 'with', 'ocr'], 'The ocr tool', 'Run nest add ocr.');
  }
  plan.content = validateSequence('verify', 'content', [['check.contradictions',
    z.strictObject({ model: z.string(), scope: z.literal('changed_and_related') })]]);
  const about = z.strictObject({ title: z.string().min(1), purpose: z.string(), out_of_scope: z.string() }).safeParse(config.about);
  if (!about.success) add('ARG_TYPE', ['about'], 'title, purpose, and out_of_scope', 'Describe the project purpose and scope.');
  const serveSettings = validateServe(config.init.type, config.serve, add);
  const models = z.record(z.string(), z.strictObject({
    provider: z.enum(['ollama', 'openai', 'strata']), kind: z.literal('chat'), base_url: z.string().optional(),
    model: z.string().nullable(), api_key_env: z.string().regex(/^[A-Za-z_][A-Za-z0-9_]*$/u).optional(),
    max_input_bytes: z.number().int().positive().optional(),
  })).safeParse(config.models);
  if (!models.success) add('ARG_TYPE', ['models'], 'Named chat models and environment variable references', 'Use models.writer from the standard template.');
  for (const [name, model] of Object.entries(models.success ? models.data : {})) {
    const url = model.base_url && URL.parse(model.base_url);
    if (url && (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.search || url.hash)) {
      add('ARG_RANGE', ['models', name, 'base_url'], 'A credential-free HTTP(S) endpoint', 'Use api_key_env for credentials.');
    }
  }
  return { errors, plan, serveSettings };
}
