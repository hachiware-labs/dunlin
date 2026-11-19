import { mkdir, readFile } from 'node:fs/promises';
import { homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { parseDocument, stringify } from 'yaml';
import { z } from 'zod';

import { atomicWrite, withWriteLock } from './authoring/files.mjs';
import { isSearchProject } from './project-serving.mjs';

const providers = { embedding: ['ollama', 'openai'], llm: ['ollama', 'openai', 'strata'] };
const modelName = z.string().trim().min(1).refine((name) => !/[\u0000-\u001f\u007f]/u.test(name));
const selectionSchema = (role) => z.strictObject({ model: modelName, provider: z.enum(providers[role]) });
export const ocrToolNames = ['oar-ocr', 'lightonocr'];
const ocrSchema = z.strictObject({ tool: z.enum(ocrToolNames) });
const globalSchema = z.strictObject({
  version: z.literal(1),
  embedding: selectionSchema('embedding').optional(),
  llm: selectionSchema('llm').optional(),
  ocr: ocrSchema.optional(),
});
const providerSettings = {
  ollama: { base_url: 'http://localhost:11434' },
  openai: { base_url: 'https://api.openai.com/v1', api_key_env: 'OPENAI_API_KEY' },
  strata: { base_url: 'http://127.0.0.1:8080/v1' },
};

function fail(code, message) {
  return Object.assign(new Error(message), { code, exitCode: 2 });
}

function globalPath() {
  return resolve(process.env.DUNLIN_GLOBAL_CONFIG || join(homedir(), '.dunlin', 'config.yaml'));
}

async function readGlobal(path) {
  let text;
  try { text = await readFile(path, 'utf8'); }
  catch (error) {
    if (error.code === 'ENOENT') return { version: 1 };
    throw error;
  }
  try {
    const document = parseDocument(text);
    if (!document.errors.length) return globalSchema.parse(document.toJS({ maxAliasCount: 0 }));
  } catch {
    // Report the allowed settings without disclosing values from the file.
  }
  throw fail('INVALID_GLOBAL_CONFIG', 'Check ' + path + '. Expected version: 1, optional embedding and llm entries with provider and model, and optional ocr with tool: oar-ocr or lightonocr.');
}

export async function configureGlobal(action, role, { provider, model, tool }) {
  const path = globalPath();
  const modelOptions = provider !== undefined || model !== undefined;
  const options = modelOptions || tool !== undefined;
  const ocr = role === 'ocr';
  if (action === 'list' && !options) return { path, ...await readGlobal(path) };
  if ((!ocr && !Object.hasOwn(providers, role)) || (action === 'unset' && options)) {
    throw fail('INVALID_ARGUMENT', 'Use config list, config set <embedding|llm> --model <name> [--provider <name>], config set ocr [--tool oar-ocr|lightonocr], or config unset <embedding|llm|ocr>.');
  }
  let selection;
  if (action === 'set') {
    if (ocr) {
      const checked = ocrSchema.safeParse({ tool: tool ?? 'oar-ocr' });
      if (modelOptions || !checked.success) throw fail('INVALID_ARGUMENT', 'Use config set ocr [--tool oar-ocr|lightonocr].');
      selection = checked.data;
    } else {
      const checked = selectionSchema(role).safeParse({ provider: provider ?? 'ollama', model });
      if (tool !== undefined || !checked.success) throw fail('INVALID_ARGUMENT',
        'Specify a nonempty --model and a --provider from ' + providers[role].join(', ') + '.');
      selection = checked.data;
    }
  }
  const folder = dirname(path);
  await mkdir(folder, { recursive: true });
  return withWriteLock(folder, async () => {
    const settings = await readGlobal(path);
    if (selection) settings[role] = selection;
    else if (Object.hasOwn(settings, role)) delete settings[role];
    else return { path, ...settings };
    await atomicWrite(path, stringify(settings));
    return { path, ...settings };
  });
}

export function projectModel(kind, { provider = 'ollama', model = null } = {}) {
  return { provider, kind, ...providerSettings[provider], model };
}

export async function initialSettings(type, values) {
  const current = await readGlobal(globalPath());
  const enabled = { embedding: isSearchProject(type), llm: type !== 'mcp' };
  const tool = values['ocr-tool'];
  if (values['no-ocr'] && (values.ocr || tool !== undefined)) throw fail('INVALID_ARGUMENT', 'Use either --ocr or --no-ocr.');
  if (tool !== undefined && !ocrToolNames.includes(tool)) throw fail('INVALID_ARGUMENT', 'Use --ocr-tool with ' + ocrToolNames.join(' or ') + '.');
  // --ocr-tool turns OCR on. The tool comes from --ocr-tool, the user settings, then oar-ocr.
  const ocr = !values['no-ocr'] && (values.ocr || tool !== undefined || type !== 'mcp' && current.ocr !== undefined);
  const selections = { ocr: ocr ? tool ?? current.ocr?.tool ?? 'oar-ocr' : null };
  for (const role of ['embedding', 'llm']) {
    const model = values[role + '-model'];
    const provider = values[role + '-provider'];
    const disabled = values['no-' + role];
    const specified = model !== undefined || provider !== undefined;
    if (!enabled[role]) {
      if (specified || disabled) throw fail('INVALID_ARGUMENT', role + ' options are unavailable for init ' + type + '.');
      continue;
    }
    if (disabled) {
      if (specified) throw fail('INVALID_ARGUMENT', 'Use --no-' + role + ' without model or provider flags for the same role.');
      continue;
    }
    if (provider !== undefined && !providers[role].includes(provider)) {
      throw fail('INVALID_ARGUMENT', 'Use --' + role + '-provider with ' + providers[role].join(', ') + '.');
    }
    const checked = model === undefined ? null : modelName.safeParse(model);
    if (checked && !checked.success) throw fail('INVALID_ARGUMENT', 'Specify a nonempty --' + role + '-model.');
    const fallback = type === 'mcp' && role === 'embedding' && !specified ? undefined : current[role];
    const selectedProvider = provider ?? fallback?.provider ?? 'ollama';
    const selectedModel = checked?.data ?? (selectedProvider === fallback?.provider ? fallback.model : undefined);
    if (!selectedModel && role === 'embedding') {
      if (provider !== undefined) throw fail('INVALID_ARGUMENT', 'Specify --embedding-model for the selected embedding provider.');
      continue;
    }
    selections[role] = { provider: selectedProvider, model: selectedModel };
  }
  return selections;
}
