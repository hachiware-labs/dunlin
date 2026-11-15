export function sourceRange(document, evidence) {
  const text = document.content;
  if (evidence.byteRange) {
    const bytes = Buffer.from(text);
    const start = [...bytes.subarray(0, evidence.byteRange.start).toString()].length;
    const body = bytes.subarray(evidence.byteRange.start, evidence.byteRange.end).toString();
    return { range: [start, start + [...body].length], text: body };
  }
  const lines = text.match(/[^\n]*\n|[^\n]+$/gu) ?? [];
  const start = [...lines.slice(0, evidence.range.startLine - 1).join('')].length;
  const body = lines.slice(evidence.range.startLine - 1, evidence.range.endLine).join('');
  return { range: [start, start + [...body].length], text: body };
}

const documentId = (source) => source.sourceId + ':' + source.objectId;
const pointer = (source) => ({ document_id: documentId(source), document_version: source.version });

export async function searchOutput(database, search, query, baseId, signal) {
  const documents = new Map();
  async function document(evidence) {
    const { source } = evidence;
    const key = JSON.stringify([source.sourceId, source.objectId, source.version]);
    if (!documents.has(key)) documents.set(key, database.getCorpusDocument(source, source.version, { signal }));
    return documents.get(key);
  }
  const results = await Promise.all(search.hits.map(async ({ title, score, evidence }, i) => {
    const snapshot = await document(evidence);
    const { source } = evidence;
    const heading = title.startsWith(source.objectId + ' — ') ? title.slice(source.objectId.length + 3) : title === source.objectId ? null : title;
    const pages = [...new Set((snapshot.locations ?? []).filter(({ range, page }) => page !== undefined
      && range.startLine <= evidence.range.endLine && range.endLine >= evidence.range.startLine).map(({ page }) => page))];
    return { rank: i + 1, score, ...pointer(source), ...sourceRange(snapshot, evidence),
      heading, page: pages.length === 1 ? pages[0] : null, evidence };
  }));
  const segments = [];
  for (const item of search.context) {
    const snapshot = await document(item.evidence);
    const location = pointer(item.evidence.source);
    for (const { kind, output_range } of item.decorations) {
      segments.push({ kind, output_range, ...(kind !== 'separator' && location) });
    }
    const { start, end } = item.body_range;
    const [sourceStart] = sourceRange(snapshot, item.evidence).range;
    segments.push({ kind: 'body', output_range: [start, end], ...location, source_range: [sourceStart, sourceStart + end - start] });
  }
  return { schema_version: 1, query, build_id: search.build_id, search_config_id: search.search_config_id, results,
    context: { text: search.context_text, chars: search.context_chars, budget_chars: search.budget_chars,
      stop_reason: search.context_stop === 'budget' ? 'budget_filled' : search.context_stop, segments },
    warnings: baseId !== search.search_config_id ? [{ code: 'SEARCH_OVERRIDES', message: 'Request overrides change the conditions used for evaluation.' }] : [] };
}
