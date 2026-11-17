import { resolveApiKey, resolveConfigValue } from './config.mjs';
import { embeddingEndpoint } from './models.mjs';

function error(code, message) {
  return Object.assign(new Error(message), { code, exitCode: code === 'VECTOR_REQUIRED' ? 3 : 2 });
}

/** The embedding model `name` under `models`, resolved for requests: `{ provider, model, url, apiKey, maxInputBytes }`. */
export function embeddingSettings(config, name) {
  const model = Object.hasOwn(config.models ?? {}, name) && config.models[name];
  if (model?.kind !== 'embedding' || !model.model) throw error('VECTOR_MODEL_REQUIRED', 'Define an embedding model under models.');
  try {
    return { ...embeddingEndpoint(model.provider, resolveConfigValue(model.model, 'model'), resolveConfigValue(model.base_url, 'base_url'),
      resolveApiKey(model.api_key_env, 'model')), ...(model.max_input_bytes !== undefined && { maxInputBytes: model.max_input_bytes }) };
  } catch { throw error('INVALID_VECTOR_MODEL', 'Check the embedding provider, model, endpoint and environment references.'); }
}
