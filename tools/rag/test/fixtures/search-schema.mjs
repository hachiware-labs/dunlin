// The project search tool lists one name for each option, with descriptions; the former server
// listed both `sourceIds` and `source_ids`, and `budgetChars` and `budget_chars`. The camelCase
// names are still accepted. This rewrites a `tools/list` result of the former server to the current
// schema so that the rest of the reply can be compared as before.
const descriptions = {
  query: 'Words or a question to search for.',
  limit: 'The most results to return.',
  source_ids: 'Search only these sources: the source IDs of the project.',
  budget_chars: 'The most characters of source context to return.',
};

/** The `tools/list` result with the project `knowledge.search` schema in its current form. */
export function currentSearchSchema(result) {
  if (!result?.tools) return result;
  return {
    ...result,
    tools: result.tools.map((tool) => {
      const properties = tool.inputSchema?.properties;
      if (tool.name !== 'knowledge.search' || !properties?.budget_chars) return tool;
      const current = {};
      for (const name of Object.keys(descriptions)) current[name] = { ...properties[name], description: descriptions[name] };
      return { ...tool, inputSchema: { ...tool.inputSchema, properties: current } };
    }),
  };
}

/** A JSON-RPC reply body, as text or an SSE `data:` line, with `currentSearchSchema` applied. */
export function currentSearchSchemaText(body) {
  return body.replace(/\{"result":\{"tools":.*?\},"jsonrpc":"2\.0","id":[^}]+\}/gu, (text) => {
    const reply = JSON.parse(text);
    return JSON.stringify({ ...reply, result: currentSearchSchema(reply.result) });
  });
}
