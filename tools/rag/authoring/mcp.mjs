import { McpServer, ResourceTemplate } from '@modelcontextprotocol/server';
import { serveStdio } from '@modelcontextprotocol/server/stdio';
import * as z from 'zod/v4';

import { paraContext, savePara } from './para.mjs';
import { ingestSource, readSource, uploadInputLimit, uploadSource } from './sources.mjs';
import { originalMimeType, originalUriTemplate, readOriginal } from './uploads.mjs';
import { affectedWiki, applyWiki, getWiki } from './wiki.mjs';

const sourceId = z.string().regex(/^[a-zA-Z0-9_.-]{1,64}$/u);
const id = z.string().min(1).max(240);
const line = z.number().int().min(1).max(2 ** 20);
const range = z.object({ startLine: line, endLine: line });
const coordinate = z.number().int().min(0).max(1000);
const shortText = z.string().trim().min(1).max(240).regex(/^[^\r\n]+$/u);
const evidence = z.object({
  source: z.object({
    sourceId, objectId: z.string().min(1), url: z.string(), version: z.string(),
    modifiedAtMs: z.number().int().nullable(),
  }),
  range,
  acquiredAtMs: z.number().int().nonnegative(),
  original: z.object({ name: z.string(), sha256: z.string().regex(/^[a-f0-9]{64}$/u), uri: z.string() }).optional(),
  locations: z.array(z.object({
    range, page: z.number().int().positive().optional(), symbol: z.string().optional(),
    bbox: z.tuple([coordinate, coordinate, coordinate, coordinate]).optional(),
  })).optional(),
});
const page = z.object({
  id, title: shortText, markdown: z.string().trim().min(1).max(100_000),
  citations: z.array(evidence).min(1).max(64),
  expectedRevision: z.string().regex(/^[a-f0-9]{64}$/u).nullable(),
});

export function serveAuthoringMcp(database, { sourceRoot, paraRoot, wikiRoot, uploadRoot, parsers, keepOriginals }) {
  return serveStdio(() => {
    const server = new McpServer({ name: 'dunlin-authoring', version: '0.1.0' });
    const register = (name, description, inputSchema, readOnlyHint, call) => {
      server.registerTool(name, { description, inputSchema, annotations: { readOnlyHint } }, async (input, ctx) => {
        const result = await call(input, ctx.mcpReq.signal);
        return { structuredContent: result, content: [{ type: 'text', text: JSON.stringify(result) }] };
      });
    };
    register('source.ingest', 'Import an HTTP(S) URL or a file in the configured source root. Returns the indexed document IDs and version.',
      z.object({ location: z.string().min(1).max(4096), keepOriginal: z.boolean().optional() }), false,
      ({ location, keepOriginal }, signal) => ingestSource(database, { sourceRoot, uploadRoot, parsers, keepOriginals, keepOriginal }, location, signal));
    register('source.upload', 'Send a whole file as Base64 or UTF-8 text. Text, Markdown and HTML are supported up to 1 MiB. Configured document/OCR parsers accept Base64 files up to 16 MiB. Returns an original resource and an upload ID.',
      z.object({
        name: z.string().min(1).max(240), content: z.string().min(1).max(uploadInputLimit),
        encoding: z.enum(['utf8', 'base64']).default('utf8'), contentType: z.string().min(1).max(128).optional(),
        originalUrl: z.string().min(1).max(4096).optional(), uploadId: sourceId.optional(),
      }), false, (input, signal) => uploadSource(database, { uploadRoot, parsers }, input, signal));
    server.registerResource('uploaded-original', new ResourceTemplate(originalUriTemplate, { list: undefined }), {
      description: 'Exact original file bytes retained at ingestion.', mimeType: originalMimeType,
    }, async (uri, { sourceId: id, sha256 }) => {
      const bytes = await readOriginal(uploadRoot, id, sha256);
      return { contents: [{ uri: uri.href, mimeType: originalMimeType, blob: bytes.toString('base64') }] };
    });
    register('source.read', 'Read source lines by document ID and optional archived version. Use the current version when saving notes or pages.',
      z.object({ sourceId, objectId: z.string().min(1).max(4096), version: z.string().regex(/^[a-f0-9]{64}$/u).optional(), startLine: line.default(1),
        maxLines: z.number().int().min(1).max(128).default(32) }), true,
      (input, signal) => readSource(database, uploadRoot, input, signal));
    register('knowledge.search', 'Find indexed source ranges by keyword.',
      z.object({ query: z.string(), sourceIds: z.array(sourceId).min(1).max(100).optional() }), true,
      (input, signal) => database.search(input.query, { sourceIds: input.sourceIds, signal }));
    if (paraRoot) {
      register('para.context', 'List existing PARA groups and note IDs, or read a single note and its revision.',
        z.object({ id: id.optional() }), true, (input) => paraContext(paraRoot, input.id));
      register('para.save', 'Save an HTML note from Markdown in a PARA category such as Projects/auth/policy. Use null for a new note or the current note revision for an update.',
        page, false, (input, signal) => savePara(paraRoot, database, uploadRoot, input, signal));
    }
    if (wikiRoot) {
      register('wiki.get', 'List the published Wiki pages and their locations, or read a single page and its revision.',
        z.object({ id: id.optional() }), true, (input) => getWiki(wikiRoot, input.id));
      register('wiki.affected', 'List Wiki pages whose cited document URL or version has changed since publication.',
        z.object({}), true, (_input, signal) => affectedWiki(wikiRoot, database, signal));
      register('wiki.apply', 'Publish authored Markdown as HTML pages with verified citations, an index, and a change log. Use null for new pages and their current revisions for updates.',
        z.object({ pages: z.array(page).min(1).max(32), summary: shortText }), false,
        (input, signal) => applyWiki(wikiRoot, database, uploadRoot, input, signal));
    }
    return server;
  });
}
