// Serves MCP over stdio with the former JavaScript server (mcp-reference.mjs).
//   library <index>    the Node API server on one index
//   project <project>  the server `nest serve` started for an MCP project, with the former search
import { readFile } from 'node:fs/promises';
import { DatabaseSync } from 'node:sqlite';
import { join } from 'node:path';

import { KnowledgeNest } from 'knowledge-nest';

import { parseConfig, validateRag } from './cli-reference/project-modules.mjs';
import { serveStdioMcp } from './mcp-reference.mjs';
import { referenceOpen, referenceSearchJson } from './search-reference.mjs';

const [mode, directory] = process.argv.slice(2);
if (mode === 'library') {
  serveStdioMcp(await KnowledgeNest.open(directory));
} else {
  const data = join(directory, '.knowledge-nest');
  const paths = { data, state: join(data, 'state.sqlite'), artifacts: join(data, 'artifacts'), corpus: join(data, 'corpus') };
  const configPath = join(directory, 'KnowledgeNestfile.yaml');
  const { config } = parseConfig(await readFile(configPath, 'utf8'));
  const { plan } = validateRag(config, configPath, null);
  const state = new DatabaseSync(paths.state, { readOnly: true });
  const build = JSON.parse(state.prepare('SELECT manifest FROM project JOIN builds ON builds.id = project.build_id').get().manifest);
  state.close();
  const database = await referenceOpen(paths, build);
  const project = { paths, config, plan };
  serveStdioMcp(database, (query, options) => referenceSearchJson(database, build, plan.search, query, project, options),
    (evidence, options) => database.read(evidence, options), false);
}
