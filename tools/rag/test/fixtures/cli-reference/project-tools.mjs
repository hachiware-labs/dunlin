import { readFile } from 'node:fs/promises';

import { KnowledgeNest } from 'knowledge-nest';

import { atomicWrite, revision, withWriteLock } from './authoring/files.mjs';
import { lightonModel } from './parsers/catalog.mjs';
import { managedOcrVersion, setupManagedOcr } from './parsers/lighton-setup.mjs';
import { oarOcrVersion, setupOarOcr } from './parsers/oar-setup.mjs';
import { hannoy, installIndexTool } from './project-index-tool.mjs';
import { isSearchProject } from './project-serving.mjs';

const lighton = { version: managedOcrVersion, modules: ['loader.ocr'], model: lightonModel, ocr: true };
export const toolsCatalog = new Map([
  ['oar-ocr', { name: 'oar-ocr', version: oarOcrVersion, modules: ['loader.ocr'], ocr: true }],
  ['lightonocr', { name: 'lightonocr', ...lighton }],
  // The former name of lightonocr, still valid in existing projects.
  ['ocr', { name: 'ocr', ...lighton, legacy: true }],
  ['hannoy', hannoy],
]);

/** The OCR tool a project records, as `{ tool, version }` with the former name read as lightonocr, or null. */
export function projectOcr(tools) {
  const entry = Array.isArray(tools) ? tools.find((item) => toolsCatalog.get(item?.name)?.ocr) : undefined;
  if (!entry) return null;
  return { tool: entry.name === 'ocr' ? 'lightonocr' : entry.name, version: entry.version };
}

/** The tools that nest add --list shows. */
export function listedTools() {
  return [...toolsCatalog.values()].filter(({ legacy }) => !legacy).map(({ ocr, legacy, ...tool }) => tool);
}

/** Checks the tools list in Rust against the bundled catalog. */
export function checkTools(config, issue) {
  if (config.tools === undefined) return;
  const catalog = [...toolsCatalog.values()].map(({ name, version }) => [name, version]);
  for (const { code, path, expected, hint } of KnowledgeNest.checkTools(config.tools, catalog)) issue(code, path, expected, hint);
}

export async function changeTool(project, requested, remove, writeTemplate) {
  const fail = (code, message, exitCode = 2) => Object.assign(new Error(message), { code, exitCode });
  const recorded = (project.config.tools ?? []).find((entry) => toolsCatalog.get(entry?.name)?.ocr);
  // nest add ocr prepares the OCR tool the project records, or the standard one.
  const name = requested === 'ocr' ? recorded?.name ?? (remove ? 'ocr' : 'oar-ocr') : requested;
  const tool = toolsCatalog.get(name);
  if (!tool) throw fail('TOOL_UNKNOWN', 'Use nest add --list to see the available tools.');
  if (!remove && tool.ocr && recorded && recorded.name !== name && !(name === 'lightonocr' && recorded.name === 'ocr')) {
    throw fail('TOOL_CONFLICT', 'A project uses one OCR tool. Run nest remove ' + recorded.name + ' first.');
  }
  if (name === 'hannoy' && !isSearchProject(project.config.init.type)) throw fail('WRONG_PROJECT_TYPE', 'The hannoy tool requires a RAG/MCP project.');
  if (remove && ['source', 'ingress', 'index', 'search'].some((section) => project.config[section]?.modules?.some(({ use, with: args }) =>
    tool.modules.includes(use) || tool.ocr && args?.ocr === 'loader.ocr'))) {
    throw fail('TOOL_IN_USE', 'Remove references to ' + name + ' modules from the KnowledgeNestfile first.');
  }
  if (!remove) {
    if (name === 'oar-ocr') await setupOarOcr();
    else if (tool.ocr) await setupManagedOcr();
    else await installIndexTool(project, tool);
  }
  const config = structuredClone(project.config);
  // Adding lightonocr replaces its former name.
  config.tools = (config.tools ?? []).filter((entry) => entry.name !== name && !(name === 'lightonocr' && entry.name === 'ocr'));
  if (!remove) config.tools.push({ name, version: tool.version });
  if (!config.tools.length) delete config.tools;
  await withWriteLock(project.paths.data, async () => {
    const current = await readFile(project.configPath, 'utf8');
    if (revision(current) !== project.configDigest) throw fail('CONFIG_CHANGED', 'The KnowledgeNestfile changed. Retry the tool operation.', 4);
    await atomicWrite(project.configPath, writeTemplate(config));
  });
  return { name, version: tool.version, configured: !remove, ...(remove ? {} : { modules: tool.modules,
    configuration: tool.configuration ?? { use: 'loader.auto', with: { unsupported: 'error', ocr: 'loader.ocr' } } }) };
}
