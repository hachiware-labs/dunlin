// The JavaScript MCP server before it moved to Rust (spec 28.10 step 3c), on the official
// TypeScript SDK. Kept to check that the Rust server publishes the same tools and answers the same
// way. Compiled from js/mcp.ts.
import { McpServer } from '@modelcontextprotocol/server';
import { serveStdio } from '@modelcontextprotocol/server/stdio';
import * as z from 'zod/v4';
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
    byteRange: z.object({ start: z.number().int().nonnegative(), end: z.number().int().positive() }).optional(),
});
const sourceIds = z.array(z.string().regex(/^[a-zA-Z0-9_.-]{1,64}$/u)).min(1).max(100).optional();
export function serveStdioMcp(database, search, read, searchOnly = false) {
    return serveStdio(() => knowledgeServer(database, search, read, searchOnly));
}
export function knowledgeServer(database, search, read, searchOnly = false) {
    const server = new McpServer({ name: 'dunlin', version: '0.5.0' });
    server.registerTool('knowledge.search', {
        description: search ? 'Retrieve indexed evidence and assemble source context using the project search settings.'
            : 'Find indexed source ranges using keyword search.',
        inputSchema: z.object({ query: z.string(), limit: z.number().int().min(1).max(100).optional(), sourceIds,
            ...(search && { budgetChars: z.number().int().min(1).max(1000000).optional(),
                budget_chars: z.number().int().min(1).max(1000000).optional(), source_ids: sourceIds }) }),
        annotations: { readOnlyHint: true },
    }, async ({ query, limit, sourceIds, budgetChars, budget_chars, source_ids }, ctx) => {
        if (budgetChars !== undefined && budget_chars !== undefined || sourceIds !== undefined && source_ids !== undefined) {
            return { isError: true, content: [{ type: 'text', text: 'Specify each search option once.' }] };
        }
        const budget = budgetChars ?? budget_chars;
        const options = { limit, sourceIds: sourceIds ?? (Array.isArray(source_ids) ? source_ids : undefined),
            budgetChars: typeof budget === 'number' ? budget : undefined, signal: ctx.mcpReq.signal };
        const result = search ? await search(query, options) : await database.search(query, options);
        return response({ ...result });
    });
    if (searchOnly)
        return server;
    server.registerTool('knowledge.read', {
        description: 'Read the indexed lines identified by evidence from knowledge.search.',
        inputSchema: z.object({ evidence }),
        annotations: { readOnlyHint: true },
    }, async ({ evidence }, ctx) => {
        const options = { signal: ctx.mcpReq.signal };
        const result = await (read ? read(evidence, options) : database.read(evidence, options));
        return response({ ...result });
    });
    if (search)
        return server;
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
    return server;
}
function response(result) {
    return {
        content: [{ type: 'text', text: JSON.stringify(result) }],
        structuredContent: result,
    };
}
