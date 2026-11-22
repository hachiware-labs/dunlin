// Serves a RAG project over HTTP with the former JavaScript server (rag-http-reference.mjs), the
// former search, answers and MCP handler, on the serving build. Prints the address as serve does.
//   node rag-http-reference-serve.mjs <project>
import { readFile } from 'node:fs/promises';
import { DatabaseSync } from 'node:sqlite';
import { join } from 'node:path';

import { createMcpHandler } from '@modelcontextprotocol/server';

import { parseConfig, validateRag } from './cli-reference/project-modules.mjs';
import { serveAddress } from './cli-reference/project-serving.mjs';
import { referenceAnswer, referenceAnswerModel } from './answer-reference.mjs';
import { knowledgeServer } from './mcp-reference.mjs';
import { referenceServeRagHttp } from './rag-http-reference.mjs';
import { referenceOpen, referenceSearchJson } from './search-reference.mjs';

const [directory] = process.argv.slice(2);
const data = join(directory, '.knowledge-nest');
const paths = { data, state: join(data, 'state.sqlite'), artifacts: join(data, 'artifacts'), corpus: join(data, 'corpus') };
const configPath = join(directory, 'KnowledgeNestfile.yaml');
const { config } = parseConfig(await readFile(configPath, 'utf8'));
const { plan, serveSettings } = validateRag(config, configPath, null);
const settings = { ...serveSettings, port: 0 };
const state = new DatabaseSync(paths.state, { readOnly: true });
const build = JSON.parse(state.prepare('SELECT manifest FROM project JOIN builds ON builds.id = project.build_id').get().manifest);
state.close();
const database = await referenceOpen(paths, build);
const project = { paths, config, plan };
const model = settings.endpoints.includes('responses') ? referenceAnswerModel(project, { optional: true }) : null;
const search = (query, options) => referenceSearchJson(database, build, plan.search, query, project, options);
const mcp = settings.endpoints.includes('mcp') ? createMcpHandler(() => knowledgeServer(database, search, undefined, true), {
  legacy: 'stateless', responseMode: 'json', maxRequestBodySize: settings.limits.max_request_bytes,
}) : null;
const server = await referenceServeRagHttp(settings, { search, model: model?.model, mcp,
  answer: async (query, signal) => referenceAnswer(project, await search(query, { signal }), query, model, signal) });
console.error('Knowledge-Nest rag: ' + serveAddress(settings.host, server.address().port) + '/');
