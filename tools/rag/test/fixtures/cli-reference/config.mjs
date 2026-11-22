// Values in KnowledgeNestfile.yaml that refer to environment variables, such as `${OPENAI_BASE_URL}` and
// `api_key_env`.

const envReference = /\$\{([A-Za-z_][A-Za-z0-9_]*)\}/gu;

function nonempty(value, name) {
  if (typeof value !== 'string' || !value.trim() || value.includes('\0')) throw new Error(`Expected ${name} to be a nonempty string`);
}

export function resolveConfigValue(value, field, allowMissing = false) {
  if (value === undefined) return;
  let missing;
  const resolved = value.replace(envReference, (_reference, name) => {
    const variable = process.env[name];
    if (variable === undefined) missing ??= name;
    return variable ?? '';
  });
  if (missing) {
    if (allowMissing) return;
    throw new Error(`Environment variable ${missing} is required by ${field}`);
  }
  if (allowMissing && !resolved.trim()) return;
  nonempty(resolved, field);
  return resolved;
}

export function resolveApiKey(name, field) {
  if (name === undefined) return;
  const value = process.env[name];
  if (!value?.trim()) throw new Error(`Environment variable ${name} is required by ${field}.apiKeyEnv`);
  if (!/^[!-~]+$/u.test(value)) throw new Error(`Environment variable ${name} contains an invalid API key for ${field}.apiKeyEnv`);
  return value;
}
