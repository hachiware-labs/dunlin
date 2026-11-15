import { Readable } from 'node:stream';
import { pipeline } from 'node:stream/promises';
import { z } from 'zod';
import { HttpError, listenHttp, sendJson } from './project-serving.mjs';

const queryText = z.string().refine((query) => Boolean(query.trim()));
const searchRequest = z.strictObject({
  query: queryText,
  limit: z.number().int().min(1).max(100).optional(),
  budget_chars: z.number().int().min(1).max(1000000).optional(),
  source_ids: z.array(z.string().regex(/^[a-zA-Z0-9_.-]{1,64}$/u)).min(1).max(100).optional(),
});
const inputMessage = z.strictObject({
  type: z.literal('message').optional(), role: z.enum(['user', 'assistant', 'system', 'developer']),
  content: z.union([z.string(), z.array(z.strictObject({ type: z.enum(['input_text', 'output_text']), text: z.string() }))]),
});
const answerRequest = z.strictObject({
  input: z.union([queryText, z.array(inputMessage).min(1)]),
  model: queryText.optional(), stream: z.literal(false).optional(), store: z.literal(false).optional(), background: z.literal(false).optional(),
});

export async function serveRagHttp(settings, { search, answer, model, mcp }) {
  const enabled = new Set(settings.endpoints);
  return listenHttp(settings, async (request, response, context) => {
    const { signal, url } = context;
    const route = { '/v1/responses': 'responses', '/v1/search': 'search', '/mcp': 'mcp' }[url.pathname];
    if (!enabled.has(route)) throw new HttpError(404, 'NOT_FOUND', 'The endpoint is unavailable.');
    if (route === 'mcp') {
      if (!['GET', 'POST', 'DELETE'].includes(request.method)) {
        response.setHeader('Allow', 'GET, POST, DELETE');
        throw new HttpError(405, 'METHOD_NOT_ALLOWED', 'Use GET, POST or DELETE for MCP.');
      }
      const headers = new Headers();
      for (const [key, value] of Object.entries(request.headers)) {
        if (typeof value === 'string' && key !== 'authorization' && key !== 'cookie') headers.set(key, value);
      }
      const body = request.method === 'POST' ? context.body : undefined;
      const result = await mcp.fetch(new Request(url, { method: request.method, headers, body, signal }));
      signal.throwIfAborted();
      response.writeHead(result.status, Object.fromEntries(result.headers));
      if (result.body) await pipeline(Readable.fromWeb(result.body), response, { signal });
      else response.end();
      return;
    }
    if (request.method !== 'POST') {
      response.setHeader('Allow', 'POST');
      throw new HttpError(405, 'METHOD_NOT_ALLOWED', 'Use POST for this endpoint.');
    }
    const body = jsonBody(request, context.body);
    if (route === 'search') {
      const { query, limit, budget_chars, source_ids } = parse(searchRequest, body);
      const result = await search(query, { limit, budgetChars: budget_chars, sourceIds: source_ids, signal });
      signal.throwIfAborted();
      sendJson(response, 200, result);
      return;
    }
    const { input, model: requested } = parse(answerRequest, body);
    if (model === undefined) throw new HttpError(503, 'MODEL_REQUIRED', 'Set the answer model in the Dunlinfile and restart serve.');
    if (requested !== undefined && requested !== model) throw new HttpError(400, 'MODEL_MISMATCH', 'Use the model configured for this project.');
    const query = question(input);
    const result = await answer(query, signal);
    signal.throwIfAborted();
    sendJson(response, 200, result);
  });
}

function jsonBody(request, bytes) {
  if (!/^application\/json(?:\s*;|$)/iu.test(request.headers['content-type'] ?? '')) {
    throw new HttpError(415, 'CONTENT_TYPE', 'Use application/json.');
  }
  try { return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes)); }
  catch { throw new HttpError(400, 'INVALID_JSON', 'Provide valid JSON.'); }
}

function parse(schema, body) {
  const parsed = schema.safeParse(body);
  if (!parsed.success) throw new HttpError(400, 'INVALID_REQUEST', 'The request contains invalid or unsupported fields.');
  return parsed.data;
}

function question(input) {
  if (typeof input === 'string') return input;
  const message = input.findLast(({ role }) => role === 'user');
  const content = message?.content;
  if (Array.isArray(content) && content.some(({ type }) => type !== 'input_text')) {
    throw new HttpError(400, 'INVALID_INPUT', 'The last user message must contain input text.');
  }
  const text = typeof content === 'string' ? content : content?.map(({ text }) => text).join('\n');
  if (!text?.trim()) throw new HttpError(400, 'INVALID_INPUT', 'Provide a nonempty user message.');
  return text;
}
