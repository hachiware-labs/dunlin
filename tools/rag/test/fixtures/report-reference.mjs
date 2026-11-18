// The reports of tools/rag/project-eval.mjs before spec 28.11 step 4b moved them to Rust: report
// show, list and compare, the evaluation status, and the misses that dictionary suggestions read.
// Tests compare the Rust implementation with it.
import { revision } from '../../authoring/files.mjs';

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

function dataset(db, ref) {
  const id = ref?.replace(/^eval-data:/u, '');
  let row = id && db.prepare('SELECT data FROM evaluation_datasets WHERE id = ?').get(id);
  if (!row && id && db.prepare("SELECT name FROM sqlite_master WHERE name = 'evaluation_dataset_tags'").get()) {
    row = db.prepare(`SELECT data FROM evaluation_datasets JOIN evaluation_dataset_tags ON id = dataset_id WHERE name = ?`).get(id);
  }
  if (!row) throw missing('Select evaluation data with eval-data add, import, or use.');
  return JSON.parse(row.data);
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

export function referenceEvaluationStatus(db, buildId, searchId, project) {
  const reference = project.config['eval-data']?.use;
  if (!reference || !project.evalSettings) return { state: 'unevaluated', report: null };
  let selected;
  try { selected = dataset(db, reference).id; }
  catch (error) { if (error.code !== 'EVAL_DATA_REQUIRED') throw error; return { state: 'unevaluated', report: null }; }
  const reports = db.prepare(`SELECT data FROM evaluation_reports
    WHERE json_extract(data, '$.dataset_id') = ? AND json_extract(data, '$.method_id') = ?
       AND json_extract(data, '$.split') = 'dev' ORDER BY rowid DESC`).all(selected, methodId(project.evalSettings));
  const matches = reports.map(({ data }) => JSON.parse(data));
  const matching = matches.find((item) => item.build_id === buildId && item.search_config_id === searchId);
  const latest = matching ?? matches[0];
  return { state: matching ? 'evaluated' : latest ? 'stale' : 'unevaluated',
    report: latest ? { id: latest.id, build_id: latest.build_id, search_config_id: latest.search_config_id, scores: latest.scores,
      unresolved: latest.rows.reduce((sum, row) => sum + row.unresolved.length, 0) } : null };
}

function reportByRef(db, ref) {
  const row = db.prepare('SELECT data FROM evaluation_reports WHERE id = ? OR tag = ? LIMIT 1').get(ref, ref);
  if (!row) throw new EvaluationError(3, 'REPORT_REQUIRED', 'Unknown report: ' + ref);
  return JSON.parse(row.data);
}

export function referenceDictionaryEvaluation(db, project, ref) {
  const addHint = 'Add a development evaluation question with dunlin eval-data add --question <text> --doc <path> --quote <text>.';
  let data;
  let report;
  if (ref !== undefined) {
    report = reportByRef(db, ref);
    if (report.split !== 'dev') throw invalid('Dictionary suggestions require a development report.');
    data = dataset(db, report.dataset_id);
  } else {
    const selected = project.config['eval-data']?.use;
    if (!selected) throw missing(addHint);
    data = dataset(db, selected);
    if (!evaluationCases(data, 'dev', project.evalSettings?.includeGenerated).length) throw missing(addHint);
    const row = db.prepare(`SELECT data FROM evaluation_reports WHERE json_extract(data, '$.dataset_id') = ?
      AND json_extract(data, '$.split') = 'dev' ORDER BY rowid DESC LIMIT 1`).get(data.id);
    if (!row) throw new EvaluationError(3, 'REPORT_REQUIRED', 'Run dunlin eval to create a development report first.');
    report = JSON.parse(row.data);
  }
  return { report, data };
}

export async function* referenceDictionaryMisses(db, report, data, database) {
  const row = db.prepare('SELECT data FROM evidence_maps WHERE id = ?').get(report.evidence_map_id);
  if (!row) throw new EvaluationError(3, 'MAP_REQUIRED', 'Run dunlin eval to record the evidence positions first.');
  const { positions } = JSON.parse(row.data);
  const cases = new Map(data.cases.map((item) => [item.id, item]));
  const primary = primaryRecall(Object.keys(report.scores));
  const documents = new Map();
  for (const result of report.rows) {
    if (result.scores[primary] === 1) continue;
    const { id, question, evidence } = cases.get(result.id);
    for (const [i, item] of evidence.entries()) {
      const saved = result.evidence?.[i];
      if (saved?.recovered) continue;
      const finding = { case_id: id, question, doc: item.doc, quote: item.quote };
      if (!saved && result.scores[primary] > 0) {
        yield { ...finding, reason: 'legacy_mixed_recovery' };
        break;
      }
      if (saved?.reason) {
        yield { ...finding, reason: saved.reason };
        continue;
      }
      const separator = item.doc.indexOf(':');
      if (!documents.has(item.doc)) documents.set(item.doc,
        await database.getDocument({ sourceId: item.doc.slice(0, separator), objectId: item.doc.slice(separator + 1) }));
      const document = documents.get(item.doc);
      const positionDocument = document && !saved && item.page ? await loadEvidenceMetadata(database, document) : document;
      const position = positionDocument && positions[saved?.position_key ?? positionKey(item, positionDocument)];
      if (!position?.range) {
        yield { ...finding, reason: position?.reason ?? 'source_missing' };
        continue;
      }
      const { start, end } = position.range;
      const bytes = Buffer.from(document.content);
      const lineStart = start === 0 ? 0 : bytes.lastIndexOf(10, start - 1) + 1;
      const lineEnd = bytes.indexOf(10, end);
      yield { ...finding, url: document.evidence.source.url, in_results: saved?.in_results,
        before: bytes.subarray(lineStart, start).toString(), text: bytes.subarray(start, end).toString(),
        after: bytes.subarray(end, lineEnd < 0 ? bytes.length : lineEnd).toString() };
    }
  }
}

export function referenceReports(db, action, refs, split) {
  if (action === 'show') return reportByRef(db, refs[0]);
  if (action === 'list') {
    const ranked = db.prepare("SELECT data FROM evaluation_reports WHERE json_extract(data, '$.split') = ? ORDER BY rowid DESC").all(split ?? 'dev')
      .map(({ data }) => JSON.parse(data)).map(({ rows, ...value }) => ({ ...value, cases: rows.length }))
      .sort((a, b) => a.dataset_id.localeCompare(b.dataset_id) || a.method_id.localeCompare(b.method_id)
        || (b.scores['evidence_recall@context'] ?? -1) - (a.scores['evidence_recall@context'] ?? -1));
    const groups = new Map();
    for (const result of ranked) {
      const key = JSON.stringify([result.dataset_id, result.method_id, result.split]);
      if (!groups.has(key)) groups.set(key, []);
      groups.get(key).push(result);
    }
    return [...groups.values()].map((results) => {
      const settings = results.map(({ construction, search_settings, budget_chars }) => ({ ...flatten({ ...construction, search: search_settings }), budget_chars }));
      const fields = [...new Set(settings.flatMap((item) => Object.keys(item)))].filter((field) => field === 'budget_chars'
        || settings.some((item) => item[field] !== settings[0][field]));
      return { dataset_id: results[0].dataset_id, method_id: results[0].method_id, split: results[0].split, columns: fields,
        results: results.map(({ construction, search_settings, ...item }, i) => ({ ...item,
          settings: Object.fromEntries(fields.map((field) => [field, settings[i][field] ?? null])) })) };
    });
  }
  const a = reportByRef(db, refs[0]);
  const b = reportByRef(db, refs[1]);
  const right = new Map(b.rows.map((row) => [row.id, row]));
  const paired = a.rows.flatMap((row) => right.has(row.id) ? [[row, right.get(row.id)]] : []);
  const conditionKeys = ['build_id', 'source_id', 'search_config_id', 'dataset_id', 'method_id', 'split', 'budget_chars'];
  const differences = Object.fromEntries(conditionKeys.filter((key) => a[key] !== b[key]).map((key) => [key, [a[key], b[key]]]));
  const scoreNames = Object.keys(a.scores).filter((name) => Object.hasOwn(b.scores, name));
  const scores = Object.fromEntries(scoreNames.map((name) => {
    const deltas = paired.map(([left, right]) => right.scores[name] - left.scores[name]);
    const counts = deltas.reduce((acc, value) => {
      acc[value > 0 ? 'wins' : value < 0 ? 'losses' : 'ties']++;
      return acc;
    }, { wins: 0, losses: 0, ties: 0 });
    const groups = new Map();
    paired.forEach(([row], i) => {
      const values = groups.get(row.group) ?? [];
      values.push(deltas[i]);
      groups.set(row.group, values);
    });
    const interval = confidenceInterval([...groups.values()]);
    return [name, { delta: mean(deltas), ...counts, interval_95: interval,
      uncertain: interval ? interval[0] <= 0 && interval[1] >= 0 : null }];
  }));
  const comparable = paired.length === a.rows.length && paired.length === b.rows.length
    && !['dataset_id', 'method_id', 'split', 'budget_chars'].some((key) => key in differences);
  const comparison = { a: a.id, b: b.id, differences, common_cases: paired.length, comparable, provisional: 'source_id' in differences };
  if (!comparable) return { ...comparison, scores: {}, common_case_scores: scores };
  if (a.split !== 'dev') return { ...comparison, scores };
  const primary = primaryRecall(scoreNames);
  const questions = new Map(dataset(db, a.dataset_id).cases.map(({ id, question }) => [id, question]));
  const cases = paired.flatMap(([before, after]) => {
    const baseline = before.scores[primary];
    const trial = after.scores[primary];
    if (baseline === undefined || trial === undefined || baseline === 1 && trial === 1) return [];
    return [{ id: before.id, question: questions.get(before.id), before: baseline, after: trial, delta: trial - baseline,
      status: trial < baseline ? 'worse' : trial === 1 ? 'recovered' : 'still_missing' }];
  });
  return { ...comparison, scores, case_metric: primary, cases };
}

function flatten(value, prefix = '') {
  if (value === null || value === undefined) return {};
  return Object.fromEntries(Object.entries(value).flatMap(([key, child]) => {
    const name = prefix ? prefix + '.' + key : key;
    if (child && typeof child === 'object') return Object.entries(flatten(child, name));
    return [[name, child]];
  }));
}

function confidenceInterval(groups) {
  if (groups.length < 5) return null;
  const summaries = groups.map((values) => [values.reduce((sum, value) => sum + value, 0), values.length]);
  let seed = 0x7254b13;
  const draws = [];
  for (let i = 0; i < 1000; i++) {
    let sum = 0;
    let count = 0;
    for (let j = 0; j < summaries.length; j++) {
      seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
      const [value, size] = summaries[Math.floor(seed / 2 ** 32 * summaries.length)];
      sum += value;
      count += size;
    }
    draws.push(sum / count);
  }
  draws.sort((a, b) => a - b);
  return [draws[25], draws[974]];
}
