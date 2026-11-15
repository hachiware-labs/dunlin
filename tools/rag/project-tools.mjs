import { readFile } from 'node:fs/promises';

import { Dunlin } from 'dunlin';

import { atomicWrite, revision, withWriteLock } from './authoring/files.mjs';
import { lightonModel } from './parsers/catalog.mjs';
import { managedOcrVersion, setupManagedOcr } from './parsers/lighton-setup.mjs';
import { hannoy, installIndexTool } from './project-index-tool.mjs';
import { isSearchProject } from './project-serving.mjs';

export const toolsCatalog = new Map([
  ['ocr', { name: 'ocr', version: managedOcrVersion, modules: ['loader.ocr'], model: lightonModel }],
  ['hannoy', hannoy],
]);

/** Checks the tools list in Rust against the bundled catalog. */
export function checkTools(config, issue) {
  if (config.tools === undefined) return;
  const catalog = [...toolsCatalog.values()].map(({ name, version }) => [name, version]);
  for (const { code, path, expected, hint } of Dunlin.checkTools(config.tools, catalog)) issue(code, path, expected, hint);
}

export async function changeTool(project, name, remove, writeTemplate) {
  const tool = toolsCatalog.get(name);
  const fail = (code, message, exitCode = 2) => Object.assign(new Error(message), { code, exitCode });
  if (!tool) throw fail('TOOL_UNKNOWN', 'Use dunlin add --list to see the available tools.');
  if (name === 'hannoy' && !isSearchProject(project.config.init.type)) throw fail('WRONG_PROJECT_TYPE', 'The hannoy tool requires a RAG/MCP project.');
  if (remove && ['source', 'ingress', 'index', 'search'].some((section) => project.config[section]?.modules?.some(({ use, with: args }) =>
    tool.modules.includes(use) || name === 'ocr' && args?.ocr === 'loader.ocr'))) {
    throw fail('TOOL_IN_USE', 'Remove references to ' + name + ' modules from the Dunlinfile first.');
  }
  if (!remove) {
    if (name === 'ocr') await setupManagedOcr();
    else await installIndexTool(project, tool);
  }
  const config = structuredClone(project.config);
  config.tools = (config.tools ?? []).filter((entry) => entry.name !== name);
  if (!remove) config.tools.push({ name, version: tool.version });
  if (!config.tools.length) delete config.tools;
  await withWriteLock(project.paths.data, async () => {
    const current = await readFile(project.configPath, 'utf8');
    if (revision(current) !== project.configDigest) throw fail('CONFIG_CHANGED', 'The Dunlinfile changed. Retry the tool operation.', 4);
    await atomicWrite(project.configPath, writeTemplate(config));
  });
  return { name, version: tool.version, configured: !remove, ...(remove ? {} : { modules: tool.modules,
    configuration: tool.configuration ?? { use: 'loader.auto', with: { unsupported: 'error', ocr: 'loader.ocr' } } }) };
}
