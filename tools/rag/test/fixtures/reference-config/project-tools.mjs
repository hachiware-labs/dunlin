// The JavaScript configuration validation that the Rust validator replaced, copied for tests.
import { readFile } from 'node:fs/promises';

import { atomicWrite, revision, withWriteLock } from '../cli-reference/authoring/files.mjs';
import { lightonModel } from '../cli-reference/parsers/catalog.mjs';
import { managedOcrVersion, setupManagedOcr } from '../cli-reference/parsers/lighton-setup.mjs';
import { hannoy, installIndexTool } from '../cli-reference/project-index-tool.mjs';
import { isSearchProject } from './project-serving.mjs';

export const toolsCatalog = new Map([
  ['ocr', { name: 'ocr', version: managedOcrVersion, modules: ['loader.ocr'], model: lightonModel }],
  ['hannoy', hannoy],
]);

export function checkTools(config, issue) {
  if (config.tools === undefined) return;
  if (!Array.isArray(config.tools)) {
    issue('ARG_TYPE', ['tools'], 'A list of tool names and versions', 'Use dunlin add --list.');
    return;
  }
  const names = new Set();
  for (const [i, entry] of config.tools.entries()) {
    const tool = toolsCatalog.get(entry?.name);
    if (!tool || tool.version !== entry.version || Object.keys(entry).some((key) => !['name', 'version'].includes(key)) || names.has(entry.name)) {
      issue('REQUIREMENT_MISSING', ['tools', i], 'A unique tool and version from the bundled catalog', 'Use dunlin add --list.');
    } else names.add(entry.name);
  }
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
