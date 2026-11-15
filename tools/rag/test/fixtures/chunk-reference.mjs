// The JavaScript chunker that the native chunker replaced. Tests compare the native output with it.
const graphemes = new Intl.Segmenter('und', { granularity: 'grapheme' });

export function referenceChunks({ content, format, corpus }, { size, overlap, structure = false, unicode = 'nfkc' }) {
  const boundaries = structure && new Set((corpus?.locations ?? []).filter(({ page }) => page).map(({ range }) => range.startLine));
  const sections = structure && new Set(content.split('\n').flatMap((text, i) =>
    /^\s{0,3}#{1,6}\s+\S/u.test(text) ? [i + 1] : []));
  const units = [];
  let byte = 0;
  let line = 1;
  for (const { segment, index } of graphemes.segment(content)) {
    units.push({ index, byte, line, count: [...segment].length, breakAfter: /[\n。.!?]$/u.test(segment) });
    byte += Buffer.byteLength(segment);
    if (segment.endsWith('\n')) line++;
  }
  if (!units.length) return [];
  units.push({ index: content.length, byte, line });
  const chunks = [];
  const last = units.length - 1;
  for (let start = 0, end = 0; end < last;) {
    const previous = end;
    let length = 0;
    end = start;
    while (end < last && (end === start || length + units[end].count <= size)) {
      const line = units[end].line;
      if (structure && end > start && line !== units[end - 1].line && (boundaries.has(line) || sections.has(line))) break;
      length += units[end++].count;
    }
    while (end <= previous && end < last) end++;
    const min = Math.max(start, previous, end - Math.floor((end - start) / 4));
    if (!structure && end < last) {
      for (let candidate = end - 1; candidate > min; candidate--) {
        if (units[candidate - 1].breakAfter) { end = candidate; break; }
      }
    }
    const first = units[start];
    const next = units[end];
    const text = content.slice(first.index, next.index);
    const heading = format === 'markdown' ? text.match(/^\s{0,3}#{1,6} +([^\r\n]+)/mu)?.[1]?.trim() ?? '' : '';
    chunks.push({
      heading,
      range: { startLine: first.line, endLine: units[end - 1].line },
      byteRange: { start: first.byte, end: next.byte },
      ...(unicode === 'nfkc' && { searchText: text.normalize('NFKC') }),
    });
    let tail = 0;
    let nextStart = end;
    while (nextStart > start + 1 && tail + units[nextStart - 1].count <= overlap) tail += units[--nextStart].count;
    start = nextStart;
  }
  return chunks;
}
