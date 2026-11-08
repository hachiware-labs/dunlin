import { McpServer } from '@modelcontextprotocol/server';
import { serveStdio } from '@modelcontextprotocol/server/stdio';
import * as z from 'zod/v4';

import type { Dunlin } from './index.js';
import type { McpHandle } from './types.js';

const evidence = z.object({
  source: z.object({
    sourceId: z.string(),
    objectId: z.string(),
    url: z.string(),
    version: z.string(),
    modifiedAtMs: z.number().int().nullable(),
  }),
  range: z.object({ startLine: z.number().int().positive(), endLine: z.number().int().positive() }),
  acquiredAtMs: z.number().int().nonnegative(),
});

const sourceIds = z.array(z.string().regex(/^[a-zA-Z0-9_.-]{1,64}$/u)).min(1).max(100).optional();

export function serveStdioMcp(database: Dunlin): McpHandle {
  return serveStdio(() => {
    const server = new McpServer({ name: 'dunlin', version: '0.5.0' });
    server.registerTool('knowledge.search', {
      description: 'Find indexed source ranges using keyword search.',
      inputSchema: z.object({ query: z.string(), limit: z.number().int().min(1).max(100).optional(), sourceIds }),
      annotations: { readOnlyHint: true },
    }, async ({ query, limit, sourceIds }, ctx) => {
      const result = await database.search(query, { limit, sourceIds, signal: ctx.mcpReq.signal });
      return response({ ...result });
    });
    server.registerTool('knowledge.read', {
      description: 'Read the indexed lines identified by evidence from knowledge.search.',
      inputSchema: z.object({ evidence }),
      annotations: { readOnlyHint: true },
    }, async ({ evidence }, ctx) => {
      const result = await database.read(evidence, { signal: ctx.mcpReq.signal });
      return response({ ...result });
    });
    server.registerTool('knowledge.explore', {
      description: 'Collect evidence by searching indexed sources and following their Markdown links.',
      inputSchema: z.object({
        query: z.string(),
        sourceIds,
        seedLimit: z.number().int().min(1).max(100).optional(),
        maxSteps: z.number().int().min(1).max(100).optional(),
        timeoutMs: z.number().int().min(1).max(60_000).optional(),
      }),
      annotations: { readOnlyHint: true },
    }, async ({ query, ...options }, ctx) => {
      const result = await database.explore(query, { ...options, signal: ctx.mcpReq.signal });
      return response({ ...result });
    });
    server.registerTool('knowledge.wiki_get', {
      description: 'Read a derived Wiki page and its currently valid citations.',
      inputSchema: z.object({ id: z.string().min(1).max(128) }),
      annotations: { readOnlyHint: true },
    }, async ({ id }, ctx) => {
      const page = await database.wiki.get(id, { signal: ctx.mcpReq.signal });
      return response({ page });
    });
    return server;
  });
}

function response<T extends Record<string, unknown>>(result: T) {
  return {
    content: [{ type: 'text' as const, text: JSON.stringify(result) }],
    structuredContent: result,
  };
}
