const documentId = (source) => source.sourceId + ':' + source.objectId;
const pointer = (source) => ({ document_id: documentId(source), document_version: source.version });

export async function searchOutput(database, search, query, baseId, signal) {
  // Rust reads each document once and returns only the excerpts and their character offsets.
  // Context assembly already describes them; other callers ask for them here.
  const evidence = [...search.hits.map((hit) => hit.evidence), ...search.context.map((item) => item.evidence)];
  const excerpts = search.excerpts ? [...search.excerpts.hits, ...search.excerpts.context]
    : evidence.length ? await database.describeEvidence(evidence, { signal }) : [];
  const excerpt = (index) => {
    if (!excerpts[index]) throw new Error('The saved revision of ' + documentId(evidence[index].source) + ' is missing.');
    return excerpts[index];
  };
  const results = search.hits.map(({ title, score, evidence }, i) => {
    const { text, start, end, locations } = excerpt(i);
    const { source } = evidence;
    const heading = title.startsWith(source.objectId + ' — ') ? title.slice(source.objectId.length + 3) : title === source.objectId ? null : title;
    const pages = [...new Set(locations.filter(({ page }) => page !== undefined).map(({ page }) => page))];
    return { rank: i + 1, score, ...pointer(source), range: [start, end], text,
      heading, page: pages.length === 1 ? pages[0] : null, evidence };
  });
  const segments = [];
  for (const [j, item] of search.context.entries()) {
    const location = pointer(item.evidence.source);
    for (const { kind, output_range } of item.decorations) {
      segments.push({ kind, output_range, ...(kind !== 'separator' && location) });
    }
    const { start, end } = item.body_range;
    const sourceStart = excerpt(search.hits.length + j).start;
    segments.push({ kind: 'body', output_range: [start, end], ...location, source_range: [sourceStart, sourceStart + end - start] });
  }
  return { schema_version: 1, query, build_id: search.build_id, search_config_id: search.search_config_id, results,
    context: { text: search.context_text, chars: search.context_chars, budget_chars: search.budget_chars,
      stop_reason: search.context_stop === 'budget' ? 'budget_filled' : search.context_stop, segments },
    warnings: baseId !== search.search_config_id ? [{ code: 'SEARCH_OVERRIDES', message: 'Request overrides change the conditions used for evaluation.' }] : [] };
}
