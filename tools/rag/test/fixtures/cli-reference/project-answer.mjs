import { KnowledgeNest } from 'knowledge-nest';

import { resolveApiKey, resolveConfigValue } from './config.mjs';
import { generatorSettings } from './models.mjs';
import { moduleSettings } from './project-modules.mjs';
import { untilAborted } from './project-trials.mjs';

function fail(exitCode, code, message) {
  return Object.assign(new Error(message), { exitCode, code });
}

/** The answer model of the ask stage: `{ model, settings }`, with the endpoint and key resolved. */
export function answerModel(project, { optional = false } = {}) {
  const name = project.plan.ask[0].args.model;
  const setting = project.config.models[name];
  if (typeof setting.model !== 'string' || !setting.model.trim()) {
    if (optional) return null;
    throw fail(3, 'MODEL_REQUIRED', 'Set the model name in models.' + name + '.model to enable answer generation.');
  }
  try {
    const model = resolveConfigValue(setting.model, 'answer model');
    const settings = generatorSettings(setting.provider, model, resolveConfigValue(setting.base_url, 'answer endpoint'),
      resolveApiKey(setting.api_key_env, 'answer model'), { maxInputBytes: setting.max_input_bytes });
    return { model, settings };
  } catch {
    throw fail(2, 'INVALID_MODEL', 'Check the answer model, endpoint and credential environment references.');
  }
}

/** Answers a question from a search result in Rust (spec 28.10 step 3b). Returns a Responses API object (spec 27.2). */
export async function answerQuestion(project, search, query, model, signal) {
  const result = await untilAborted(KnowledgeNest.answerQuestion({ plan: moduleSettings(project.plan.ask), search, query, model: model.settings },
    { signal }), signal);
  if (result.status === 'done') return result.output;
  const { code, message, exitCode, module } = result.error;
  const error = Object.assign(new Error(message), code && { code }, exitCode && { exitCode });
  if (module) {
    error.module ??= module;
    error.section ??= 'ask';
  }
  throw error;
}

export function answerText(response) {
  const [content] = response.output[0].content;
  const sources = [...new Set(content.annotations.map(({ file_id }) => file_id))];
  return content.text + (sources.length ? '\n\nSources\n' + sources.map((id) => '- ' + id).join('\n') : '');
}
