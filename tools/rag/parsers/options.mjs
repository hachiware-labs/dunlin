import { resolveApiKey, resolveConfigEndpoint, resolveConfigPath, resolveConfigValue, setConfig } from '../config.mjs';
import { lightonModel, parserTools } from './catalog.mjs';

export function parserOptions(category, settings) {
  const field = `parsers.${category}`;
  const tool = resolveConfigValue(settings.tool, `${field}.tool`);
  if (!parserTools[category].includes(tool)) throw new Error(`Unknown ${category} tool ${tool}`);
  const remote = tool === 'lightonocr' || tool === 'docling-serve';
  if (!remote && (settings.baseUrl !== undefined || settings.apiKeyEnv !== undefined)) {
    throw new Error(`${field} endpoint settings require lightonocr or docling-serve`);
  }
  if (tool !== 'lightonocr' && settings.model !== undefined) throw new Error(`${field}.model requires lightonocr`);
  if (category !== 'documents' && settings.modelDir !== undefined) throw new Error(`${field}.modelDir is only for document conversion`);
  if (category !== 'ocr' && settings.lang !== undefined) throw new Error(`${field}.lang is only for OCR`);
  const model = tool === 'lightonocr' ? resolveConfigValue(settings.model, `${field}.model`) ?? lightonModel : undefined;
  const managed = tool === 'lightonocr' && settings.baseUrl === undefined && settings.apiKeyEnv === undefined && model === lightonModel;
  return {
    tool, managed, model,
    baseUrl: remote ? resolveConfigEndpoint(settings.baseUrl, `${field}.baseUrl`)
      ?? (tool === 'lightonocr' ? 'http://127.0.0.1:8010/v1' : 'http://127.0.0.1:5001') : undefined,
    apiKey: resolveApiKey(settings.apiKeyEnv, field),
    modelDir: resolveConfigValue(settings.modelDir, `${field}.modelDir`),
    lang: category === 'ocr' ? resolveConfigValue(settings.lang, `${field}.lang`) ?? 'ja' : undefined,
  };
}

export function configuredParsers(loaded) {
  const settings = loaded?.config.parsers;
  if (settings?.documents?.modelDir === undefined) return settings;
  const modelDir = resolveConfigPath(loaded, resolveConfigValue(settings.documents.modelDir, 'parsers.documents.modelDir'));
  return { ...settings, documents: { ...settings.documents, modelDir } };
}

export async function setupParsers(loaded, category, values) {
  if (category !== undefined && !Object.hasOwn(parserTools, category)) {
    throw new Error(`Expected a setup category: ${Object.keys(parserTools).join(', ')}`);
  }
  const overrides = { tool: values.tool, baseUrl: values['base-url'], model: values.model, lang: values.lang };
  if (values.backend !== undefined && (category !== 'ocr' || values.check)) throw new Error('--backend requires dunlin setup ocr');
  if (values.backend !== undefined && (overrides.baseUrl !== undefined || (overrides.tool !== undefined && overrides.tool !== 'lightonocr'))) {
    throw new Error('--backend requires the managed LightOnOCR runtime');
  }
  if (!category && (!values.check || Object.values(overrides).some((value) => value !== undefined))) {
    throw new Error('Specify a setup category, or run dunlin setup --check');
  }
  const reports = [];
  for (const key of category ? [category] : Object.keys(parserTools)) {
    const saved = loaded?.config.parsers?.[key];
    const tool = values.tool ?? (values.backend === undefined ? saved?.tool : 'lightonocr') ?? parserTools[key][0];
    const settings = values.backend === undefined ? { ...(saved?.tool === tool ? saved : {}), tool } : { tool, lang: saved?.lang };
    if (key === 'documents' && saved?.modelDir !== undefined) settings.modelDir = saved.modelDir;
    for (const [name, value] of Object.entries(overrides)) if (value !== undefined) settings[name] = value;
    const report = { category: key, tool, configured: Boolean(saved), ready: false };
    try {
      if (!values.check && tool === 'docling-rs') settings.modelDir ??= './.dunlin-models/docling';
      const options = parserOptions(key, settings);
      if (options.modelDir) options.modelDir = resolveConfigPath(loaded, options.modelDir);
      report.tool = options.tool;
      const module = await (key === 'documents' ? import('./documents.mjs') : key === 'ocr' ? import('./ocr.mjs') : import('./code.mjs'));
      if (values.backend !== undefined && !options.managed) throw new Error('--backend requires the managed LightOnOCR runtime');
      if (!loaded && !values.check && !options.managed) throw new Error('Configuration not found. Run dunlin init or pass --config.');
      if (options.managed && values.check) report.prepared = false;
      Object.assign(report, await module.check(options, !values.check, { backend: values.backend }));
      if (!values.check && loaded) {
        await setConfig(loaded.path, `parsers.${key}`, JSON.stringify(settings));
        report.configured = true;
      }
      report.ready = true;
    } catch (error) {
      report.error = error.message;
    }
    reports.push(report);
  }
  return reports;
}
