// The JavaScript character offsets that describeEvidence replaced. Tests compare the two.
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
