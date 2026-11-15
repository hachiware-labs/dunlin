import { randomUUID } from 'node:crypto';
import { lstat, mkdir, open, readFile, rename, rm, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { parserTools } from './parsers/catalog.mjs';

const filename = 'dunlin.json';
const envName = /^[A-Za-z_][A-Za-z0-9_]*$/u;
const envReference = /\$\{([A-Za-z_][A-Za-z0-9_]*)\}/gu;
const defaultGeneration = { provider: 'ollama', model: 'qwen3:4b' };

export async function initConfig(directory, preset = 'rag') {
  if (!['rag', 'mcp', 'para', 'llm-wiki'].includes(preset)) throw new Error('Expected --preset rag, mcp, para, or llm-wiki');
  const path = await newConfigPath(directory);
  const root = dirname(path);
  const config = { version: 1, index: './knowledge', corpus: { backend: 'sqlite' }, sources: { docs: './docs' } };
  if (preset === 'rag') {
    config.rag = {
      generation: defaultGeneration,
      embedding: { provider: 'ollama', model: 'embeddinggemma' },
      vectorStore: { type: 'sqlite' },
    };
  }
  if (preset === 'para') config.authoring = { para: './para', sourceRoot: '.' };
  if (preset === 'llm-wiki') config.authoring = { wiki: './wiki', sourceRoot: '.' };
  await mkdir(join(root, 'docs'), { recursive: true });
  if (config.authoring?.para) {
    const { preparePara } = await import('./authoring/para.mjs');
    await preparePara(join(root, 'para'));
  }
  if (config.authoring?.wiki) {
    const { prepareWiki } = await import('./authoring/wiki.mjs');
    await prepareWiki(join(root, 'wiki'));
  }
  try {
    await writeFile(path, formatConfig(config), { flag: 'wx' });
  } catch (error) {
    if (error.code !== 'EEXIST') throw error;
    throw new Error(`Configuration already exists at ${path}`);
  }
  return path;
}

export async function newConfigPath(directory) {
  const path = resolve(directory, filename);
  const existing = await lstat(path).catch((error) => { if (error.code !== 'ENOENT') throw error; });
  if (existing) throw new Error(`Configuration already exists at ${path}`);
  return path;
}

export function initialInstructions(path, preset = 'rag') {
  const docs = '`docs/` に Markdown またはテキストの資料を置きます。';
  const mcp = 'MCP クライアントの起動設定に `dunlin serve --stdio --config <設定ファイルの絶対パス>` を登録します。';
  const steps = {
    rag: ['RAG', [docs, `Ollama を起動し、回答モデル \`${defaultGeneration.model}\` を用意します。`,
      '最初は `dunlin config set rag.embedding null` でキーワード検索を使います。',
      '`dunlin ingest` で取り込み、`dunlin ask "<資料についての質問>"` で質問します。']],
    mcp: ['MCP', [docs, '`dunlin ingest` で取り込みます。', mcp,
      '接続したクライアントから `knowledge.search` で資料を検索します。']],
    para: ['PARA', [docs, mcp, '`source.ingest` に `docs/ファイル名` を渡し、`source.read` で本文を読みます。',
      '`para.save` でノートを保存し、`dunlin serve --web` で閲覧します。']],
    'llm-wiki': ['LLM Wiki', [docs, mcp, '`source.ingest` に `docs/ファイル名` を渡し、`source.read` で本文を読みます。',
      '`wiki.apply` でページを公開し、`dunlin serve --web` で閲覧します。']],
  };
  const [label, instructions] = steps[preset];
  return [
    `次の手順（${label}）`,
    `作業フォルダ ${dirname(path)}`,
    ...instructions.map((step, index) => `${index + 1}. ${step}`),
    '文書数と索引の状態は `dunlin corpus show` で確認できます。',
  ].join('\n');
}

export async function loadConfig(file, required = false) {
  if (file !== undefined) {
    nonempty(file, '--config');
    const path = resolve(file);
    const config = await readConfig(path);
    if (!config) throw new Error(`Configuration not found at ${path}`);
    return { path, config };
  }

  let directory = process.cwd();
  while (true) {
    const path = join(directory, filename);
    const config = await readConfig(path);
    if (config) return { path, config };
    const parent = dirname(directory);
    if (parent === directory) break;
    directory = parent;
  }
  if (required) throw new Error('Configuration not found. Run dunlin init or pass --config.');
}

export async function setConfig(path, key, input) {
  const sourceKey = /^(sources|keepOriginals)\.(.*)$/u.exec(key);
  const segments = sourceKey ? sourceKey.slice(1) : key.split('.');
  if (segments.length > 3 || segments.some((part) => !part)) throw new Error('Expected a configuration key');
  let value;
  try {
    value = JSON.parse(input);
  } catch {
    value = input;
  }

  const lockPath = `${path}.lock`;
  const lock = await open(lockPath, 'wx').catch((error) => {
    if (error.code !== 'EEXIST') throw error;
    throw new Error(`Configuration is locked at ${lockPath}`);
  });
  try {
    const config = await readConfig(path);
    if (!config) throw new Error(`Configuration not found at ${path}`);
    const updated = replaceValue(config, segments, value);
    validateConfig(updated);
    const temporary = `${path}.${randomUUID()}.tmp`;
    try {
      await writeFile(temporary, formatConfig(updated), { flag: 'wx' });
      await rename(temporary, path);
    } finally {
      await rm(temporary, { force: true });
    }
  } finally {
    await lock.close();
    await rm(lockPath, { force: true });
  }
}

export function resolveConfigPath(config, path) {
  return resolve(dirname(config.path), path);
}

export function resolveCorpusBackend(config) {
  const backend = resolveConfigValue(config?.corpus?.backend, 'corpus.backend') ?? 'sqlite';
  if (backend !== 'sqlite' && backend !== 'json') throw new Error('Expected corpus.backend to be sqlite or json');
  return backend;
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

export function resolveConfigEndpoint(value, field) {
  const resolved = resolveConfigValue(value, field);
  endpoint(resolved, field);
  return resolved;
}

export function resolveApiKey(name, field) {
  if (name === undefined) return;
  const value = process.env[name];
  if (!value?.trim()) throw new Error(`Environment variable ${name} is required by ${field}.apiKeyEnv`);
  if (!/^[!-~]+$/u.test(value)) throw new Error(`Environment variable ${name} contains an invalid API key for ${field}.apiKeyEnv`);
  return value;
}

export function formatConfig(config) {
  return `${JSON.stringify(config, null, 2)}\n`;
}

export function isSourceId(id) {
  return typeof id === 'string' && /^[a-zA-Z0-9_.-]{1,64}$/u.test(id);
}

async function readConfig(path) {
  let text;
  try {
    text = await readFile(path, 'utf8');
  } catch (error) {
    if (error.code === 'ENOENT') return;
    throw error;
  }
  try {
    const config = JSON.parse(text.replace(/^\uFEFF/u, ''));
    validateConfig(config);
    return config;
  } catch (error) {
    throw new Error(`Invalid configuration at ${path}: ${error.message}`, { cause: error });
  }
}

function replaceValue(object, [key, ...rest], value) {
  if (!rest.length) return { ...object, [key]: value };
  const child = Object.hasOwn(object, key) ? object[key] : {};
  if (!isObject(child)) throw new Error(`Cannot set a nested key under ${key}`);
  return { ...object, [key]: replaceValue(child, rest, value) };
}

function validateConfig(config) {
  fields(config, 'configuration', ['version', 'index', 'corpus', 'sources', 'keepOriginals', 'rag', 's3Endpoint', 'authoring', 'parsers']);
  if (config.version !== 1) throw new Error('Expected configuration version 1');
  template(config.index, 'index');
  if (config.corpus !== undefined) {
    fields(config.corpus, 'corpus', ['backend']);
    if (config.corpus.backend !== undefined) oneOf(config.corpus.backend, 'corpus.backend', ['sqlite', 'json']);
  }
  endpoint(config.s3Endpoint, 's3Endpoint', true);
  if (config.sources !== undefined) {
    if (!isObject(config.sources)) throw new Error('Expected sources to be an object');
    for (const [id, location] of Object.entries(config.sources)) {
      if (!isSourceId(id)) throw new Error(`Invalid source ID ${id}`);
      template(location, `sources.${id}`);
    }
  }
  if (config.keepOriginals !== undefined) {
    if (!isObject(config.keepOriginals)) throw new Error('Expected keepOriginals to be an object');
    for (const [id, keep] of Object.entries(config.keepOriginals)) {
      if (!isSourceId(id) || typeof keep !== 'boolean') throw new Error(`Expected keepOriginals.${id} to be a boolean for a valid source ID`);
    }
  }
  if (config.authoring !== undefined) {
    fields(config.authoring, 'authoring', ['para', 'wiki', 'sourceRoot']);
    for (const [key, value] of Object.entries(config.authoring)) template(value, `authoring.${key}`);
  }
  if (config.parsers !== undefined) {
    fields(config.parsers, 'parsers', Object.keys(parserTools));
    for (const [category, value] of Object.entries(config.parsers)) {
      const name = `parsers.${category}`;
      fields(value, name, ['tool', 'baseUrl', 'apiKeyEnv', 'model', 'lang', 'modelDir']);
      oneOf(value.tool, `${name}.tool`, parserTools[category]);
      endpoint(value.baseUrl, `${name}.baseUrl`, true);
      apiKeyEnv(value.apiKeyEnv, name);
      if (value.model !== undefined) template(value.model, `${name}.model`);
      if (value.lang !== undefined) template(value.lang, `${name}.lang`);
      if (value.modelDir !== undefined) template(value.modelDir, `${name}.modelDir`);
    }
  }
  if (config.rag === undefined) return;
  fields(config.rag, 'rag', ['generation', 'embedding', 'vectorStore']);
  if (config.rag.generation !== undefined) model(config.rag.generation, 'rag.generation', ['ollama', 'openai', 'strata']);
  if (config.rag.embedding !== undefined && config.rag.embedding !== null) {
    model(config.rag.embedding, 'rag.embedding', ['ollama', 'openai']);
  }
  const store = config.rag.vectorStore;
  if (store !== undefined) {
    fields(store, 'rag.vectorStore', ['type', 'url', 'apiKeyEnv']);
    oneOf(store.type, 'rag.vectorStore.type', ['local', 'sqlite', 'chroma', 'qdrant']);
    endpoint(store.url, 'rag.vectorStore.url', true);
    apiKeyEnv(store.apiKeyEnv, 'rag.vectorStore');
    if (store.type === 'local' || store.type === 'sqlite') {
      if (store.url !== undefined) throw new Error('rag.vectorStore.url requires Chroma or Qdrant');
      if (store.apiKeyEnv !== undefined) throw new Error('rag.vectorStore.apiKeyEnv requires Chroma or Qdrant');
    }
  }
}

function model(value, name, providers) {
  fields(value, name, ['provider', 'model', 'baseUrl', 'apiKeyEnv']);
  oneOf(value.provider, `${name}.provider`, providers);
  template(value.model, `${name}.model`);
  endpoint(value.baseUrl, `${name}.baseUrl`, true);
  apiKeyEnv(value.apiKeyEnv, name);
}

function fields(value, name, allowed) {
  if (!isObject(value)) throw new Error(`Expected ${name} to be an object`);
  for (const key of Object.keys(value)) {
    if (!allowed.includes(key)) throw new Error(`Unknown configuration field ${name}.${key}`);
  }
}

function isObject(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function nonempty(value, name) {
  if (typeof value !== 'string' || !value.trim() || value.includes('\0')) throw new Error(`Expected ${name} to be a nonempty string`);
}

function template(value, name) {
  nonempty(value, name);
  const withoutReferences = value.replace(envReference, '');
  if (withoutReferences.includes('${')) throw new Error(`Invalid environment reference in ${name}`);
  return withoutReferences !== value;
}

function apiKeyEnv(value, name) {
  if (value !== undefined && (typeof value !== 'string' || !envName.test(value))) {
    throw new Error(`Expected ${name}.apiKeyEnv to name an environment variable`);
  }
}

function oneOf(value, name, allowed) {
  if (typeof value === 'string' && template(value, name)) return;
  if (!allowed.includes(value)) throw new Error(`Expected ${name} to be one of ${allowed.join(', ')}`);
}

function endpoint(value, name, allowTemplate = false) {
  if (value === undefined) return;
  if (allowTemplate && template(value, name)) return;
  nonempty(value, name);
  const url = URL.parse(value);
  if (!url || !['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.search || url.hash) {
    throw new Error(`Expected ${name} to be an HTTP(S) URL without credentials, query, or fragment`);
  }
}
