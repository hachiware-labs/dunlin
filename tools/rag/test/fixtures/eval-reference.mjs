// The evaluation of tools/rag/project-eval.mjs before spec 28.11 step 4a moved it to Rust:
// scoring, evidence positions and evidence maps. Tests compare the Rust implementation with it.
import { readFile, writeFile } from 'node:fs/promises';
import { performance } from 'node:perf_hooks';
import { randomUUID } from 'node:crypto';

import { revision } from './cli-reference/authoring/files.mjs';

class EvaluationError extends Error {
  constructor(exitCode, code, message) {
    super(message);
    this.exitCode = exitCode;
    this.code = code;
  }
}

const invalid = (message) => new EvaluationError(2, 'INVALID_EVAL_DATA', message);
const missing = (message) => new EvaluationError(3, 'EVAL_DATA_REQUIRED', message);
const digest = (prefix, value) => prefix + '-' + revision(JSON.stringify(value));
const methodId = (settings) => digest('method', { recall: settings.recall, percentiles: settings.percentiles, include_generated: settings.includeGenerated });
const trialCheckpoint = () => {};

function dataset(db, ref) {
  const id = ref?.replace(/^eval-data:/u, '');
  let row = id && db.prepare('SELECT data FROM evaluation_datasets WHERE id = ?').get(id);
  if (!row && id && db.prepare("SELECT name FROM sqlite_master WHERE name = 'evaluation_dataset_tags'").get()) {
    row = db.prepare(`SELECT data FROM evaluation_datasets JOIN evaluation_dataset_tags ON id = dataset_id WHERE name = ?`).get(id);
  }
  if (!row) throw missing('Select evaluation data with eval-data add, import, or use.');
  return JSON.parse(row.data);
}

function normalized(text) {
  const segments = new Intl.Segmenter('und', { granularity: 'grapheme' }).segment(text);
  const map = [];
  let value = '';
  let offset = 0;
  for (const { segment } of segments) {
    const end = offset + Buffer.byteLength(segment);
    for (const char of segment.normalize('NFKC')) {
      if (/\s/u.test(char)) {
        if (!value.length || value.endsWith(' ')) {
          if (value.length) map.at(-1).end = end;
          continue;
        }
        value += ' ';
      } else value += char;
      for (let i = 0; i < char.length; i++) map.push({ start: offset, end });
    }
    offset = end;
  }
  return { value: value.trimEnd(), map };
}

function position(text, evidence, locations = []) {
  const { value, map } = normalized(text);
  const query = normalized(evidence.quote).value;
  const prefix = evidence.prefix && normalized(evidence.prefix).value;
  const suffix = evidence.suffix && normalized(evidence.suffix).value;
  if (!query) return { reason: 'empty_quote' };
  const permitted = hintedLines(text, evidence, locations);
  const newlines = [];
  if (permitted) {
    for (const [position, byte] of Buffer.from(text).entries()) if (byte === 10) newlines.push(position);
  }
  const lineOf = (offset) => {
    let low = 0;
    let high = newlines.length;
    while (low < high) {
      const middle = (low + high) >>> 1;
      if (newlines[middle] < offset) low = middle + 1;
      else high = middle;
    }
    return low + 1;
  };
  const found = [];
  for (let i = value.indexOf(query); i !== -1; i = value.indexOf(query, i + 1)) {
    if (prefix && !value.slice(0, i).trimEnd().endsWith(prefix)) continue;
    if (suffix && !value.slice(i + query.length).trimStart().startsWith(suffix)) continue;
    const range = { start: map[i].start, end: map[i + query.length - 1].end };
    if (permitted && !permitted.some(({ startLine, endLine }) =>
      startLine <= lineOf(range.start) && endLine >= lineOf(range.end - 1))) continue;
    found.push(range);
  }
  return found.length === 1 ? { range: found[0] } : found.length
    ? { reason: 'ambiguous_quote', candidates: found }
    : { reason: 'quote_not_found', candidates: [], suggestions: nearbyQuotes(text, query) };
}

function hintedLines(text, { page, heading }, locations) {
  if (page === undefined && heading === undefined) return null;
  const pageRanges = page === undefined ? null : locations.filter((location) => location.page === page).map(({ range }) => range);
  if (heading === undefined) return pageRanges;
  const lines = text.split('\n');
  const sections = [];
  const expected = heading.normalize('NFKC').trim();
  let open;
  for (const [i, line] of lines.entries()) {
    const match = /^ {0,3}(#{1,6})\s+(.+?)\s*#*\s*$/u.exec(line);
    if (!match) continue;
    if (open && match[1].length <= open.depth) {
      sections.push({ startLine: open.line, endLine: i });
      open = undefined;
    }
    if (match[2].normalize('NFKC') === expected) open ??= { depth: match[1].length, line: i + 1 };
  }
  if (open) sections.push({ startLine: open.line, endLine: lines.length });
  if (!pageRanges) return sections;
  return sections.flatMap((section) => pageRanges.map((page) => ({ startLine: Math.max(section.startLine, page.startLine),
    endLine: Math.min(section.endLine, page.endLine) }))).filter(({ startLine, endLine }) => startLine <= endLine);
}

function positionKey(evidence, document) {
  const { doc, quote, prefix, suffix, page, heading } = evidence;
  const key = ['nfkc-whitespace-v1', doc, quote, prefix, suffix, document.content];
  if (page !== undefined || heading !== undefined) key.push(page, heading, document.locations);
  return digest('pos', key);
}

async function loadEvidenceMetadata(database, document) {
  const { source } = document.evidence;
  const corpus = await database.getCorpusDocument(source, source.version);
  return { ...document, originalHash: corpus?.originalSha256, locations: corpus?.locations };
}

function nearbyQuotes(text, query) {
  const grams = (value) => {
    const units = [...value];
    return new Set(units.slice(1).map((char, i) => units[i] + char));
  };
  const expected = grams(query);
  if (expected.size < 3) return [];
  const items = [];
  let byte = 0;
  for (const line of text.match(/[^\n]*\n|[^\n]+$/gu) ?? []) {
    const end = byte + Buffer.byteLength(line);
    if (line.length <= Math.max(400, query.length * 3)) {
      const got = grams(line.normalize('NFKC').replace(/\s+/gu, ' ').trim());
      const overlap = [...got].filter((key) => expected.has(key)).length;
      const similarity = 2 * overlap / (got.size + expected.size);
      if (similarity >= 0.45) items.push({ start: byte, end: end - Number(line.endsWith('\n')), similarity });
    }
    byte = end;
  }
  return items.sort((a, b) => b.similarity - a.similarity || a.start - b.start).slice(0, 5);
}

function originalRange(content, evidence) {
  if (evidence.byteRange) return evidence.byteRange;
  const lines = content.match(/[^\n]*\n|[^\n]+$/gu) ?? [];
  const start = Buffer.byteLength(lines.slice(0, evidence.range.startLine - 1).join(''));
  return { start, end: start + Buffer.byteLength(lines.slice(evidence.range.startLine - 1, evidence.range.endLine).join('')) };
}

function covered(target, evidence, content, source, settings) {
  const ranges = evidence.filter((item) => item.source.objectId === source.objectId && item.source.sourceId === source.sourceId
    && item.source.version === source.version).map((item) => originalRange(content, item))
    .map(({ start, end }) => [Math.max(target.start, start), Math.min(target.end, end)])
    .filter(([start, end]) => start < end).sort((a, b) => a[0] - b[0]);
  const bytes = Buffer.from(content);
  const length = [...bytes.subarray(target.start, target.end).toString()].length;
  let end = target.start;
  let count = 0;
  for (const [from, to] of ranges) {
    if (to <= end) continue;
    count += [...bytes.subarray(Math.max(from, end), to).toString()].length;
    end = to;
  }
  return count >= (length <= settings.short_evidence_chars ? length : length * settings.min_coverage);
}

function mean(values) {
  return values.length ? values.reduce((sum, item) => sum + item, 0) / values.length : null;
}

function evaluationCases(data, split, includeGenerated) {
  const included = includeGenerated ? ['provided', 'reviewed', 'generated'] : ['provided', 'reviewed'];
  return data.cases.filter((item) => item.split === split && included.includes(item.state));
}

function primaryRecall(names) {
  return names.includes('evidence_recall@context') ? 'evidence_recall@context'
    : 'evidence_recall@' + Math.max(...names.map((name) => Number(name.slice('evidence_recall@'.length))));
}

export async function referenceEvaluate(db, project, build, settings, values, runId, database, search) {
  const data = dataset(db, project.config['eval-data']?.use);
  const split = values.split ?? 'dev';
  const cases = evaluationCases(data, split, settings.includeGenerated);
  if (!cases.length) throw missing('No evaluation cases in split ' + split + '.');
  if (values.tag && db.prepare('SELECT id FROM evaluation_reports WHERE tag = ?').get(values.tag)) throw invalid('Report tag already exists.');
  const positions = {};
  const lookup = db.prepare('SELECT data FROM evidence_overrides WHERE id = ?');
  const rows = [];
  let searchId;
  const getPosition = db.prepare('SELECT data FROM evidence_positions WHERE id = ?');
  const savePosition = db.prepare('INSERT OR IGNORE INTO evidence_positions VALUES (?, ?)');
  for (const item of cases) {
    trialCheckpoint();
    const elapsed = performance.now();
    const result = await search(item.question);
    searchId = result.search_config_id;
    const ms = performance.now() - elapsed;
    const matches = [];
    for (const evidence of item.evidence) {
      const separator = evidence.doc.indexOf(':');
      const sourceId = evidence.doc.slice(0, separator);
      const objectId = evidence.doc.slice(separator + 1);
      const found = await database.getDocument({ sourceId, objectId });
      const document = found && (evidence.page ? await loadEvidenceMetadata(database, found) : found);
      if (!document) {
        matches.push({ reason: 'source_missing' });
        continue;
      }
      const key = positionKey(evidence, document);
      const previous = lookup.get(key) ?? getPosition.get(key);
      const match = previous ? JSON.parse(previous.data) : position(document.content, evidence, document.locations);
      if (match.reason === 'quote_not_found' && match.suggestions === undefined) match.suggestions = nearbyQuotes(document.content, normalized(evidence.quote).value);
      if (!previous) savePosition.run(key, JSON.stringify(match));
      positions[key] = { key, doc: evidence.doc, quote: evidence.quote, version: document.evidence.source.version,
        content_hash: revision(document.content), ...match };
      matches.push({ ...match, document, position_key: key });
    }
    const ranked = result.hits.map(({ evidence }) => evidence);
    const recovered = Object.fromEntries(settings.recall.at.map((at) => {
      const hits = at === 'context' ? result.context.map(({ evidence }) => evidence) : ranked.slice(0, at);
      return ['evidence_recall@' + at, matches.map(({ range, document }) => Boolean(range
        && covered(range, hits, document.content, document.evidence.source, settings.recall)))];
    }));
    const scores = Object.fromEntries(Object.entries(recovered).map(([name, flags]) => [name, mean(flags.map(Number))]));
    const primary = recovered[primaryRecall(Object.keys(recovered))];
    const rankedMetric = settings.recall.at.find((at) => typeof at === 'number' && at >= ranked.length);
    const knownRanked = recovered['evidence_recall@' + rankedMetric];
    const evidence = split === 'dev' ? matches.map(({ position_key, range, reason, document }, i) => primary[i] ? { recovered: true }
      : { recovered: false, position_key, reason, ...(range && {
        in_results: knownRanked?.[i] ?? covered(range, ranked, document.content, document.evidence.source, settings.recall),
      }) }) : undefined;
    rows.push({ id: item.id, group: item.group, scores, complete: matches.every(({ range }) => range),
      unresolved: matches.filter(({ range }) => !range).map(({ reason }) => reason), latency_ms: ms, context_chars: result.context_chars,
      context_stop: result.context_stop, evidence });
  }
  const names = Object.keys(rows[0].scores);
  const latency = rows.map(({ latency_ms }) => latency_ms).sort((a, b) => a - b);
  const evidenceMap = saveEvidenceMap(db, data.id, build, positions);
  const unresolved = rows.reduce((sum, row) => sum + row.unresolved.length, 0);
  const short = rows.filter((row) => row.context_chars < settings.contextBudget);
  const report = {
    id: runId ?? randomUUID(), tag: values.tag ?? null, build_id: build.build_id, search_config_id: searchId,
    dataset_id: data.id, method_id: methodId(settings), evidence_map_id: evidenceMap.id,
    build_config_id: build.config_id ?? null,
    evaluation_config_id: digest('ecfg', [searchId, settings]),
    split, budget_chars: settings.contextBudget,
    source_id: build.source_id, construction: settings.construction, search_settings: settings.searchModules,
    evidence: { resolved: cases.reduce((sum, item) => sum + item.evidence.length, 0) - unresolved, unresolved,
      fully_resolved_cases: rows.filter(({ complete }) => complete).length },
    context: { average_chars: mean(rows.map(({ context_chars }) => context_chars)), short_cases: short.length,
      short_ratio: short.length / rows.length, reasons: short.reduce((counts, row) => {
        counts[row.context_stop] = (counts[row.context_stop] ?? 0) + 1;
        return counts;
      }, {}) },
    scores: Object.fromEntries(names.map((name) => [name, mean(rows.map((row) => row.scores[name]))])),
    diagnostic: Object.fromEntries(names.map((name) => [name, mean(rows.filter(({ complete }) => complete).map((row) => row.scores[name]))])),
    latency_ms: Object.fromEntries(settings.percentiles.map((p) => [p, latency[Math.max(0, Math.ceil(p * latency.length / 100) - 1)]])),
    test_runs: split === 'test' ? db.prepare("SELECT count(*) AS n FROM evaluation_reports WHERE json_extract(data, '$.dataset_id') = ? AND json_extract(data, '$.split') = 'test'").get(data.id).n + 1 : undefined,
    rows,
  };
  trialCheckpoint();
  db.prepare('INSERT INTO evaluation_reports VALUES (?, ?, ?)').run(report.id, report.tag, JSON.stringify(report));
  return report;
}

function saveEvidenceMap(db, datasetId, build, positions) {
  const value = { dataset_id: datasetId, source_id: build.source_id, positions };
  const id = digest('map', value);
  const map = { id, build_id: build.build_id, ...value };
  db.prepare('INSERT OR IGNORE INTO evidence_maps VALUES (?, ?)').run(id, JSON.stringify(map));
  return map;
}

export async function referenceEvidenceMaps(db, project, action, ref, values, database, build) {
  const getMap = (id) => {
    const row = db.prepare('SELECT data FROM evidence_maps WHERE id = ?').get(id);
    if (!row) throw new EvaluationError(3, 'MAP_REQUIRED', 'Evidence map not found: ' + id);
    return JSON.parse(row.data);
  };
  if (action === 'list') return db.prepare('SELECT id FROM evidence_maps ORDER BY rowid DESC').all();
  if (action === 'show') return getMap(ref);
  if (action === 'export') {
    if (!values.output) throw invalid('Use --output <file>.');
    const map = getMap(ref);
    await writeFile(values.output, JSON.stringify(map, null, 2) + '\n', { flag: 'wx' });
    return { id: map.id };
  }
  if (action === 'import' || action === 'confirm') {
    let input;
    if (action === 'import') {
      try { input = JSON.parse(await readFile(ref, 'utf8')); }
      catch { throw invalid('Expected an exported evidence map JSON file.'); }
    } else {
      if (!values.key || !/^\d+$/u.test(values.start ?? '') || !/^\d+$/u.test(values.end ?? '') || !values.reviewer?.trim()) {
        throw invalid('Use --key <position-id> --start <byte> --end <byte> --reviewer <name>.');
      }
      input = { id: ref, positions: { [values.key]: { range: { start: Number(values.start), end: Number(values.end) } } } };
    }
    const previous = getMap(input.id);
    if (action === 'confirm' && !Object.hasOwn(previous.positions, values.key)) throw invalid('Unknown position ID.');
    if (previous.source_id !== build.source_id) throw invalid('Select a build for the same source snapshot using --build.');
    const edits = [];
    for (const [key, original] of Object.entries(previous.positions)) {
      const provided = input.positions?.[key];
      if (!provided || action === 'import' && JSON.stringify(provided.range) === JSON.stringify(original.range)) continue;
      const colon = original.doc.indexOf(':');
      const document = await database.getDocument({ sourceId: original.doc.slice(0, colon), objectId: original.doc.slice(colon + 1) });
      const bytes = Buffer.from(document?.content ?? '');
      const { start, end } = provided.range ?? {};
      if (!Number.isInteger(start) || !Number.isInteger(end) || start < 0 || end <= start || end > bytes.length
        || (bytes[start] & 0xc0) === 0x80 || (bytes[end] & 0xc0) === 0x80
        || revision(document.content) !== original.content_hash) throw invalid('Invalid byte range for ' + original.doc);
      edits.push([key, { range: { start, end }, confirmed: true,
        confirmation: { reviewer: values.reviewer ?? null, at: new Date().toISOString() } }]);
    }
    db.exec('BEGIN IMMEDIATE');
    try {
      for (const [key, entry] of edits) db.prepare('INSERT INTO evidence_overrides VALUES (?, ?) ON CONFLICT(id) DO UPDATE SET data = excluded.data')
        .run(key, JSON.stringify(entry));
      const positions = { ...previous.positions };
      for (const [key, entry] of edits) {
        const { reason, candidates, ...original } = previous.positions[key];
        positions[key] = { ...original, ...entry };
      }
      const result = saveEvidenceMap(db, previous.dataset_id, build, positions);
      db.exec('COMMIT');
      return result;
    } catch (error) { db.exec('ROLLBACK'); throw error; }
  }
  const selected = dataset(db, project.config['eval-data']?.use);
  const positions = {};
  for (const item of selected.cases) for (const evidence of item.evidence) {
    const colon = evidence.doc.indexOf(':');
    const found = await database.getDocument({ sourceId: evidence.doc.slice(0, colon), objectId: evidence.doc.slice(colon + 1) });
    const document = found && (evidence.page ? await loadEvidenceMetadata(database, found) : found);
    if (!document) continue;
    const key = positionKey(evidence, document);
    const saved = db.prepare('SELECT data FROM evidence_overrides WHERE id = ?').get(key);
    positions[key] = { key, doc: evidence.doc, quote: evidence.quote, version: document.evidence.source.version,
      content_hash: revision(document.content), ...(saved ? JSON.parse(saved.data) : position(document.content, evidence, document.locations)) };
  }
  return saveEvidenceMap(db, selected.id, build, positions);
}
