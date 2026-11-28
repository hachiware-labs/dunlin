// The project search tool lists one name for each option, with descriptions; the former server
// listed both `sourceIds` and `source_ids`, and `budgetChars` and `budget_chars`. The camelCase
// names are still accepted. The project tools now also take `format` (search) and `ref`, `around`
// and `max_chars` (read), and describe the documents of the project, so their descriptions come
// from the server under test. This rewrites a `tools/list` result of the former server to the
// current tools so that the rest of the reply can be compared as before.
const descriptions = {
  query: 'Words or a question to search for.',
  limit: 'The most results to return.',
  source_ids: 'Search only these sources: the source IDs of the project.',
  budget_chars: 'The most characters of source context to return.',
};
const format = {
  type: 'string',
  enum: ['text', 'json'],
  description: 'text (the default): the passages found as refs, and the text of the best ones. json: the full result, with scores and evidence.',
};
const read = {
  ref: {
    type: 'string', minLength: 1, maxLength: 1100,
    description: 'A ref from knowledge.search, such as docs:guide.md#L20-45; or any lines of a document, such as docs:guide.md#L1-200; or docs:guide.md for all of it.',
  },
  around: { type: 'integer', minimum: 0, maximum: 1000, description: 'Lines to read before and after the ref too.' },
  max_chars: { type: 'integer', minimum: 100, maximum: 200000, description: 'The most characters to return (default 20000).' },
};

/** The `tools/list` result with the project tools in their current form; `actual` is the
 * `tools/list` result of the server under test, whose descriptions are taken. */
export function currentSearchSchema(result, actual) {
  if (!result?.tools) return result;
  const project = result.tools.some((tool) => tool.name === 'knowledge.search' && tool.inputSchema?.properties?.budget_chars);
  const describe = (tool) => actual?.tools?.find(({ name }) => name === tool.name)?.description ?? tool.description;
  return {
    ...result,
    tools: result.tools.map((tool) => {
      const properties = tool.inputSchema?.properties;
      if (project && tool.name === 'knowledge.search') {
        const current = {};
        for (const name of Object.keys(descriptions)) current[name] = { ...properties[name], description: descriptions[name] };
        current.format = format;
        return { ...tool, description: describe(tool), inputSchema: { ...tool.inputSchema, properties: current } };
      }
      if (project && tool.name === 'knowledge.read') {
        const { required: _required, ...schema } = tool.inputSchema;
        return { ...tool, description: describe(tool), inputSchema: { ...schema, properties: { ...read, evidence: properties.evidence } } };
      }
      return tool;
    }),
  };
}

const toolsReply = /\{"result":\{"tools":.*?\},"jsonrpc":"2\.0","id":[^}]+\}/gu;

/** A JSON-RPC reply body, as text or an SSE `data:` line, with `currentSearchSchema` applied;
 * `actual` is the body the server under test sent. */
export function currentSearchSchemaText(body, actual = '') {
  const actualTools = [...actual.matchAll(toolsReply)].map(([text]) => JSON.parse(text).result);
  let at = 0;
  return body.replace(toolsReply, (text) => {
    const reply = JSON.parse(text);
    return JSON.stringify({ ...reply, result: currentSearchSchema(reply.result, actualTools[at++]) });
  });
}
