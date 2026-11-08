#!/usr/bin/env node
import { parseArgs } from 'node:util';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { realpath } from 'node:fs/promises';
import { resolve } from 'node:path';

import { Dunlin } from 'dunlin';

import {
  formatConfig, initConfig, initialInstructions, isSourceId, loadConfig, resolveApiKey, resolveConfigEndpoint,
  resolveConfigPath, resolveConfigValue, setConfig,
} from './config.mjs';
import { createEmbedder, createGenerator } from './models.mjs';
import { configuredParsers } from './parsers/options.mjs';
import { answerQuestion } from './rag.mjs';
import { createVectorStore } from './vector-stores.mjs';
import { buildVectors, searchVectors } from './vectors.mjs';

const embeddingOptions = {
  'embedding-provider': { type: 'string' }, 'embedding-model': { type: 'string' },
  'embedding-base-url': { type: 'string' },
  'vector-store': { type: 'string' }, 'vector-store-url': { type: 'string' },
};
const embeddingUsage = '--embedding-provider <ollama|openai> --embedding-model <name> [--embedding-base-url <url>] [--vector-store <local|sqlite|chroma|qdrant>] [--vector-store-url <url>]';
const configOption = { config: { type: 'string' } };
const usage = `Usage:
  dunlin init [directory] [--preset <rag|mcp|para|llm-wiki>]
  dunlin config show [--config <file>]
  dunlin config set [--config <file>] <key> <value>
  dunlin corpus show [--config <file>] [--index <directory>]
  dunlin corpus set [--config <file>] [--index <directory>] <name|description|metadata.key> <value>
  dunlin setup [<documents|ocr|code>] [--tool <tool>] [--check] [--config <file>]
               [--base-url <url>] [--model <model>] [--lang <language>] [--backend <cuda|rocm|mps>]
  dunlin serve-ocr [--device <gpu|cpu>] [--python <executable>] [--model-dir <directory>] [--port <port>]
  dunlin serve-docling [--config <file>] [--model-dir <directory>] [--port <port>]
  dunlin serve [--stdio] [--web [--port <port>]] [--config <file>] [--index <directory>]
  dunlin ingest [--config <file>] [--index <directory>] [--source <id>] [--keep-original] [--s3-endpoint <url>] [path-or-url]
  dunlin embed [--config <file>] [--index <directory>] [${embeddingUsage}]
  dunlin ask [--config <file>] [--index <directory>] [--provider <openai|ollama|strata>] [--model <name>] [--source <id> ...] [--base-url <url>] [${embeddingUsage}] [--query <query>] [--json] <question>`;

function modelSettings(configured, field, provider, model, baseUrl) {
  const configuredProvider = resolveConfigValue(configured?.provider, `${field}.provider`, provider !== undefined);
  const useConfigured = provider === undefined || provider === configuredProvider;
  return {
    provider: provider ?? configuredProvider,
    model: model ?? (useConfigured ? resolveConfigValue(configured?.model, `${field}.model`) : undefined),
    baseUrl: baseUrl ?? (useConfigured ? resolveConfigEndpoint(configured?.baseUrl, `${field}.baseUrl`) : undefined),
    apiKey: useConfigured ? resolveApiKey(configured?.apiKeyEnv, field) : undefined,
  };
}

function embeddingConfig(index, values, rag, required = false) {
  if (!required && !rag?.embedding && Object.keys(embeddingOptions).every((key) => values[key] === undefined)) return;
  const { provider, model, baseUrl, apiKey } = modelSettings(rag?.embedding, 'rag.embedding',
    values['embedding-provider'], values['embedding-model'], values['embedding-base-url']);
  if (!provider || !model?.trim()) throw new Error('Set rag.embedding or specify --embedding-provider and --embedding-model');
  const configured = rag?.vectorStore;
  const configuredType = resolveConfigValue(configured?.type, 'rag.vectorStore.type', values['vector-store'] !== undefined);
  const backend = values['vector-store'] ?? configuredType;
  const useConfigured = backend === configuredType;
  const storeUrl = values['vector-store-url'] ?? (useConfigured ? resolveConfigEndpoint(configured?.url, 'rag.vectorStore.url') : undefined);
  const storeKey = useConfigured ? resolveApiKey(configured?.apiKeyEnv, 'rag.vectorStore') : undefined;
  const embedder = createEmbedder(provider, model, baseUrl, apiKey);
  return { embedder, store: createVectorStore(index, embedder, backend, storeUrl, storeKey) };
}

function indexDirectory(explicit, loaded) {
  if (explicit !== undefined) {
    if (explicit.trim()) return explicit;
  } else if (loaded) {
    return resolveConfigPath(loaded, resolveConfigValue(loaded.config.index, 'index'));
  }
  throw new Error('Set index in dunlin.json or pass --index');
}

async function withDatabase(index, run) {
  const database = await Dunlin.open(index);
  try {
    return await run(database);
  } finally {
    await database.close();
  }
}

async function serveOcr({ device = 'gpu', python, 'model-dir': modelDir, port = '8010' }) {
  if (!['gpu', 'cpu'].includes(device)) throw new Error('Expected --device gpu or cpu');
  if (!/^\d{1,5}$/u.test(port) || Number(port) > 65535) throw new Error('Expected --port from 0 to 65535');
  const { lightonServer, managedOcr } = await import('./parsers/lighton-setup.mjs');
  const managed = python === undefined ? await managedOcr({ required: device === 'gpu', requireModel: modelDir === undefined }) : undefined;
  const executable = python ?? managed?.python ?? (process.platform === 'win32' ? 'python' : 'python3');
  const directory = modelDir === undefined ? managed?.modelDir : resolve(modelDir);
  const args = ['-I', '-u', lightonServer, '--device', device, '--port', port];
  if (directory !== undefined) args.push('--model-dir', directory);
  const child = spawn(executable, args, { stdio: 'inherit', windowsHide: true });
  const [code] = await once(child, 'exit').catch((error) => {
    throw new Error(`Could not start Python at ${executable}: ${error.message}`, { cause: error });
  });
  process.exitCode = code ?? 1;
}

async function serve(index, loaded, { stdio, web, port }) {
  const settings = loaded?.config.authoring;
  const authoring = settings?.para || settings?.wiki;
  if (web && !authoring) throw new Error('Set authoring.para or authoring.wiki in dunlin.json to serve the Web viewer');
  const database = await Dunlin.open(index);
  let http;
  try {
    if (!authoring) {
      await database.serveMcp({ transport: 'stdio' });
      return;
    }
    const path = (key) => resolveConfigPath(loaded, resolveConfigValue(settings[key], `authoring.${key}`) ?? '.');
    const { preparePara } = await import('./authoring/para.mjs');
    const { prepareWiki } = await import('./authoring/wiki.mjs');
    const roots = {
      paraRoot: settings.para && await preparePara(path('para')),
      wikiRoot: settings.wiki && await prepareWiki(path('wiki')),
      uploadRoot: resolve(index, '.dunlin-uploads-v1'),
    };
    const sourceRoot = stdio && await realpath(path('sourceRoot'));
    if (web) {
      const { startAuthoringWeb } = await import('./authoring/web.mjs');
      http = await startAuthoringWeb(database, roots, port);
    }
    if (stdio) {
      const { serveAuthoringMcp } = await import('./authoring/mcp.mjs');
      serveAuthoringMcp(database, { sourceRoot, ...roots, parsers: configuredParsers(loaded), keepOriginals: loaded.config.keepOriginals });
      if (http) process.stdin.once('end', () => http.close(() => database.close().catch(console.error)));
    }
    if (http) {
      const { address, port: boundPort } = http.address();
      console.error(`Dunlin Web: http://${address}:${boundPort}/`);
    }
  } catch (error) {
    http?.close();
    await database.close();
    throw error;
  }
}

async function ingest(index, sourceId, location, s3Endpoint, loaded, retainOriginal) {
  const parsers = configuredParsers(loaded);
  const keepOriginal = retainOriginal || loaded?.config.keepOriginals?.[sourceId] === true;
  const { decodeRemote, ingestFiles } = await import('./ingest-files.mjs');
  if (location.startsWith('s3://')) {
    const { importDocuments } = await import('../import/import-documents.mjs');
    const { parserCategory } = await import('./parsers/parse.mjs');
    return importDocuments(index, sourceId, [location], s3Endpoint, {
      decode: (body, type, name) => decodeRemote(body, type, name, { parsers, keepOriginal }),
      supports: (key) => parserCategory(parsers, key),
    });
  }
  return withDatabase(index, async (database) => {
    const uploadRoot = resolve(index, '.dunlin-uploads-v1');
    if (/^https?:\/\//iu.test(location)) {
      const { ingestSource } = await import('./authoring/sources.mjs');
      return ingestSource(database, { sourceId, uploadRoot, parsers, keepOriginal }, location);
    }
    return ingestFiles(database, { sourceId, uploadRoot, parsers, keepOriginal }, location);
  });
}

function ingestEndpoint(locations, explicit, loaded) {
  if (locations.some((location) => location.startsWith('s3://'))) {
    return explicit ?? resolveConfigEndpoint(loaded?.config.s3Endpoint, 's3Endpoint');
  }
}

try {
  const [command, ...args] = process.argv.slice(2);
  if (command === '--help' || command === '-h') {
    console.log(usage);
  } else if (command === 'init') {
    const { values, positionals } = parseArgs({ args, allowPositionals: true, options: { preset: { type: 'string' } } });
    if (positionals.length > 1 || positionals[0] === '') throw new Error(usage);
    const directory = positionals[0] ?? '.';
    let preset = values.preset;
    if (args.length === 0 && process.stdin.isTTY && process.stdout.isTTY && process.stderr.isTTY) {
      const { promptInitPreset } = await import('./init-prompt.mjs');
      preset = await promptInitPreset(directory);
    }
    if (preset === null) {
      console.error('初期化を中止しました。');
    } else {
      const path = await initConfig(directory, preset);
      console.log(path);
      console.error(initialInstructions(path, preset));
    }
  } else if (command === 'config') {
    const [action, ...rest] = args;
    const { values, positionals } = parseArgs({ args: rest, allowPositionals: true, options: configOption });
    if (action === 'show' && positionals.length === 0) {
      const { config } = await loadConfig(values.config, true);
      process.stdout.write(formatConfig(config));
    } else if (action === 'set' && positionals.length === 2) {
      const { path } = await loadConfig(values.config, true);
      await setConfig(path, ...positionals);
      console.log(`Updated ${path}`);
    } else {
      throw new Error(usage);
    }
  } else if (command === 'corpus') {
    const [action, ...rest] = args;
    const { values, positionals } = parseArgs({ args: rest, allowPositionals: true, options: {
      ...configOption, index: { type: 'string' },
    } });
    const loaded = await loadConfig(values.config);
    const index = indexDirectory(values.index, loaded);
    const { describeCorpus, setCorpusInfo } = await import('./corpus.mjs');
    if (action === 'show' && positionals.length === 0) {
      const rag = loaded?.config.rag;
      const vectorStore = rag?.embedding && (() => embeddingConfig(index, {}, rag).store);
      process.stdout.write(formatConfig(await describeCorpus(index, loaded, vectorStore)));
    } else if (action === 'set' && positionals.length === 2) {
      console.log(`Updated ${await setCorpusInfo(index, ...positionals)}`);
    } else {
      throw new Error(usage);
    }
  } else if (command === 'setup') {
    const { values, positionals } = parseArgs({ args, allowPositionals: true, options: {
      ...configOption, tool: { type: 'string' }, check: { type: 'boolean' },
      'base-url': { type: 'string' }, model: { type: 'string' }, lang: { type: 'string' }, backend: { type: 'string' },
    } });
    if (positionals.length > 1) throw new Error(usage);
    const loaded = await loadConfig(values.config, !values.check && positionals[0] !== 'ocr');
    const { setupParsers } = await import('./parsers/options.mjs');
    const results = await setupParsers(loaded, positionals[0], values);
    console.log(JSON.stringify({ platform: process.platform, arch: process.arch, results }, null, 2));
    if (results.some(({ ready }) => !ready)) process.exitCode = 1;
  } else if (command === 'serve-ocr') {
    const { values } = parseArgs({ args, options: {
      device: { type: 'string' }, python: { type: 'string' }, 'model-dir': { type: 'string' }, port: { type: 'string' },
    } });
    await serveOcr(values);
  } else if (command === 'serve-docling') {
    const { values } = parseArgs({ args, options: { ...configOption, 'model-dir': { type: 'string' }, port: { type: 'string' } } });
    const port = values.port ?? '5001';
    if (!/^\d{1,5}$/u.test(port) || Number(port) > 65535) throw new Error('Expected --port from 0 to 65535');
    const loaded = await loadConfig(values.config);
    const modelDir = values['model-dir'] === undefined ? configuredParsers(loaded)?.documents?.modelDir : resolve(values['model-dir']);
    const { startDoclingService } = await import('./parsers/docling-service.mjs');
    const server = await startDoclingService({ modelDir, port: Number(port) });
    console.log(`Dunlin Docling: http://${server.address().address}:${server.address().port}`);
  } else if (command === 'serve') {
    const { values } = parseArgs({ args, options: {
      stdio: { type: 'boolean' }, web: { type: 'boolean' }, port: { type: 'string' }, index: { type: 'string' }, ...configOption,
    } });
    const port = values.port === undefined ? 8787 : Number(values.port);
    if (values.port !== undefined && (!values.web || !/^\d{1,5}$/u.test(values.port) || port > 65535)) {
      throw new Error('Use --port with --web and specify an integer from 0 to 65535');
    }
    const loaded = await loadConfig(values.config);
    await serve(indexDirectory(values.index, loaded), loaded, { stdio: values.stdio || !values.web, web: values.web, port });
  } else if (command === 'ingest') {
    const { values, positionals } = parseArgs({
      args, allowPositionals: true,
      options: { index: { type: 'string' }, source: { type: 'string' }, 'keep-original': { type: 'boolean' }, 's3-endpoint': { type: 'string' }, ...configOption },
    });
    const loaded = await loadConfig(values.config);
    const index = indexDirectory(values.index, loaded);
    if (values.source !== undefined && !isSourceId(values.source)) throw new Error('Invalid --source ID');
    if (positionals.length === 1 && values.source) {
      const endpoint = ingestEndpoint(positionals, values['s3-endpoint'], loaded);
      console.log(JSON.stringify(await ingest(index, values.source, positionals[0], endpoint, loaded, values['keep-original'])));
    } else if (!positionals.length && loaded) {
      const entries = Object.entries(loaded.config.sources ?? {})
        .filter(([id]) => !values.source || values.source === id)
        .map(([id, location]) => [id, resolveConfigValue(location, `sources.${id}`)]);
      if (!entries.length) throw new Error('No matching sources configured');
      const endpoint = ingestEndpoint(entries.map(([, location]) => location), values['s3-endpoint'], loaded);
      const results = [];
      for (const [sourceId, location] of entries) {
        const target = /^(?:s3|https?):\/\//iu.test(location) ? location : resolveConfigPath(loaded, location);
        results.push({ sourceId, ...await ingest(index, sourceId, target, endpoint, loaded, values['keep-original']) });
      }
      console.log(JSON.stringify(results));
    } else {
      throw new Error(usage);
    }
  } else if (command === 'embed') {
    const { values } = parseArgs({ args, options: { index: { type: 'string' }, ...embeddingOptions, ...configOption } });
    const loaded = await loadConfig(values.config);
    const index = indexDirectory(values.index, loaded);
    const { embedder, store } = embeddingConfig(index, values, loaded?.config.rag, true);
    const outcome = await withDatabase(index, (database) => buildVectors(database, embedder, store));
    console.log(JSON.stringify(outcome));
  } else if (command === 'ask') {
    const { values, positionals } = parseArgs({
      args, allowPositionals: true,
      options: {
        index: { type: 'string' }, provider: { type: 'string' }, model: { type: 'string' },
        source: { type: 'string', multiple: true }, 'base-url': { type: 'string' }, query: { type: 'string' },
        json: { type: 'boolean' }, ...embeddingOptions, ...configOption,
      },
    });
    if (positionals.length !== 1 || !positionals[0].trim()) throw new Error(usage);
    const loaded = await loadConfig(values.config);
    const index = indexDirectory(values.index, loaded);
    const rag = loaded?.config.rag;
    const { provider, model, baseUrl, apiKey } = modelSettings(rag?.generation, 'rag.generation', values.provider, values.model, values['base-url']);
    if (!provider || !model?.trim()) throw new Error('Set rag.generation or specify --provider and --model');
    const generate = createGenerator(provider, model, baseUrl, apiKey);
    const vector = embeddingConfig(index, values, rag);
    const result = await withDatabase(index, (database) => {
      const retrieve = vector && ((query, options) => searchVectors(database, vector.embedder, vector.store, query, options));
      return answerQuestion(database, positionals[0], {
        sourceIds: values.source, query: values.query, retrieve,
      }, generate);
    });
    if (values.json) {
      console.log(JSON.stringify(result));
    } else {
      console.log(result.answer);
      for (const { id, url, range, content } of result.citations) {
        console.log(`\n[${id}] ${url} (${range.startLine}–${range.endLine})\n${content.trimEnd()}`);
      }
    }
  } else {
    throw new Error(usage);
  }
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
