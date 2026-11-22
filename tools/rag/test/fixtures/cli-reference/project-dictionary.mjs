import { readFile, realpath } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';

import { KnowledgeNest } from 'knowledge-nest';
import { parseDocument } from 'yaml';

import { atomicWrite, revision } from './authoring/files.mjs';

export class DictionaryError extends Error {
  constructor(exitCode, code, message) {
    super(message);
    this.exitCode = exitCode;
    this.code = code;
  }
}

const termSurface = /^[\p{L}\p{N}][\p{L}\p{M}\p{N}_./・×+#-]*[\p{L}\p{M}\p{N}]$/u;
const maxSurfaceLength = 16;
const analysisBatchSize = 32;
const csvField = (value) => /[",\r\n]/u.test(value) ? '"' + value.replaceAll('"', '""') + '"' : value;
const slice = (bytes, start, end) => bytes.subarray(start, end).toString();
const searchTerms = (tokens) => tokens.flatMap(({ term }) => term === null ? [] : [term]);
const segmentation = (tokens) => tokens.map(({ surface, term }) => ({ surface, term }));

function tokenSurfaces(bytes, tokens) {
  return tokens.map((token) => ({ ...token, surface: slice(bytes, token.start, token.end) }));
}

function candidatesForEvidence(question, evidence) {
  const starts = new Map(evidence.tokens.map(({ start }, i) => [start, i]));
  const ends = new Map(evidence.tokens.map(({ end }, i) => [end, i + 1]));
  const evidenceTerms = new Set(searchTerms(evidence.tokens));
  const candidates = new Map();
  for (const [i, first] of question.tokens.entries()) {
    for (let j = i; j < question.tokens.length; j++) {
      const last = question.tokens[j];
      const surface = slice(question.bytes, first.start, last.end);
      if ([...surface].length > maxSurfaceLength || /\s/u.test(surface)) break;
      if (!termSurface.test(surface) || !/\p{L}/u.test(surface) || candidates.has(surface)) continue;
      const queryTokens = question.tokens.slice(i, j + 1);
      const queryTerms = searchTerms(queryTokens);
      if (queryTerms.length && queryTerms.every((term) => evidenceTerms.has(term))) continue;
      const length = last.end - first.start;
      for (let at = evidence.bytes.indexOf(surface, evidence.start); at >= 0 && at + length <= evidence.end;
        at = evidence.bytes.indexOf(surface, at + 1)) {
        if (!starts.has(at) || !ends.has(at + length)) continue;
        const evidenceTokens = evidence.tokens.slice(starts.get(at), ends.get(at + length));
        const documentTerms = searchTerms(evidenceTokens);
        if (!queryTerms.length && !documentTerms.length) continue;
        // Drop shared tokens at either edge so a phrase such as a term followed by an action
        // does not become a dictionary entry when only the term's segmentation differs.
        if ([0, -1].some((edge) => {
          const q = queryTokens.at(edge);
          const d = evidenceTokens.at(edge);
          return q.surface === d.surface && q.term === d.term;
        })) continue;
        const split = queryTokens.length >= evidenceTokens.length ? queryTokens : evidenceTokens;
        const fragment = split.findLast(({ term, surface }) => term && [...surface].length > 1)
          ?? split.findLast(({ term }) => term);
        const part = fragment && fragment.surface !== surface ? fragment.surface : [...surface].slice(1).join('');
        const reading = split.every((token) => token.reading) ? split.map((token) => token.reading).join('') : '*';
        candidates.set(surface, { surface, csv: [surface, '名詞', reading].map(csvField).join(','), start: first.start, end: last.end,
          question_tokens: segmentation(queryTokens), evidence_tokens: segmentation(evidenceTokens),
          queries: { whole: question.bytes.toString(), partial: slice(question.bytes, 0, first.start) + part + slice(question.bytes, last.end) } });
        break;
      }
    }
  }
  // Retain the smallest spans whose boundaries differ. Wider overlapping spans otherwise
  // propose attaching surrounding prose to the same term.
  const ordered = [...candidates.values()].sort((a, b) => (a.end - a.start) - (b.end - b.start));
  return ordered.filter(({ start, end }, i) => !ordered.slice(0, i).some((part) => start <= part.start && part.end <= end));
}

async function* analyzeMisses(misses, report, dictionary) {
  const normalizeQuery = report.search_settings.find(({ use }) => use === 'query.normalize')?.with.unicode === 'nfkc';
  const normalizeDocument = report.construction?.transform?.find(({ use }) => use === 'text.normalize')?.with.unicode === 'nfkc';
  const normalize = (value, enabled) => enabled ? value.normalize('NFKC') : value;
  const batch = [];
  async function* analyzeBatch() {
    const texts = [...new Set(batch.flatMap((item) => item.context ? [item.context.query, item.context.body] : []))];
    const analyses = texts.length ? await KnowledgeNest.analyzeJapanese(texts, dictionary) : [];
    const tokens = new Map(texts.map((text, i) => [text, analyses[i]]));
    for (const { miss, context } of batch) {
      if (!context) {
        yield { miss };
        continue;
      }
      const { query, body, start, end } = context;
      const queryBytes = Buffer.from(query);
      const documentBytes = Buffer.from(body);
      yield { miss, question: { bytes: queryBytes, tokens: tokenSurfaces(queryBytes, tokens.get(query)) },
        evidence: { bytes: documentBytes, start, end,
          tokens: tokenSurfaces(documentBytes, tokens.get(body).filter((token) => token.start < end && token.end > start)) } };
    }
    batch.length = 0;
  }
  for await (const miss of misses) {
    let context;
    if (!miss.reason) {
      const prefix = normalize(miss.before, normalizeDocument);
      const excerpt = normalize(miss.text, normalizeDocument);
      const start = Buffer.byteLength(prefix);
      context = { query: normalize(miss.question.trim(), normalizeQuery), body: prefix + excerpt + normalize(miss.after, normalizeDocument),
        start, end: start + Buffer.byteLength(excerpt) };
    }
    batch.push({ miss, context });
    if (batch.length === analysisBatchSize) yield* analyzeBatch();
  }
  if (batch.length) yield* analyzeBatch();
}

export async function suggestDictionary(report, misses, dictionary) {
  if (!report.search_settings.some(({ use }) => use === 'retrieval.keyword')) {
    throw new DictionaryError(2, 'KEYWORD_SEARCH_REQUIRED', 'Choose a report that used Lindera keyword search.');
  }
  const usesContext = Object.hasOwn(report.scores, 'evidence_recall@context');
  const findings = [];
  const candidates = new Map();
  const classes = { dictionary: new Set(), partial_match: new Set(), other: new Set(), unresolved: new Set() };
  for await (const { miss, question, evidence } of analyzeMisses(misses, report, dictionary)) {
    const { case_id, question: raw, doc, quote, url, reason, in_results } = miss;
    const finding = { case_id, question: raw, doc, quote, ...(url && { url }) };
    const record = (classification, details) => {
      classes[classification].add(raw);
      if (details) findings.push({ ...finding, classification, ...details });
    };
    if (reason) {
      record('unresolved', { reason });
      continue;
    }
    const proposed = candidatesForEvidence(question, evidence);
    if (proposed.length) {
      record('dictionary');
      for (const entry of proposed) {
        if (!candidates.has(entry.surface)) candidates.set(entry.surface, { surface: entry.surface, csv: entry.csv, examples: [] });
        const { surface, question_tokens, evidence_tokens, queries } = entry;
        candidates.get(surface).examples.push({ ...finding, question_tokens, evidence_tokens, queries });
      }
      continue;
    }
    const queryTerms = [...new Set(searchTerms(question.tokens))];
    const evidenceTerms = searchTerms(evidence.tokens);
    if (queryTerms.some((term) => !evidenceTerms.includes(term) && evidenceTerms.some((longer) => longer.includes(term)))) {
      record('partial_match');
      continue;
    }
    const matched = queryTerms.filter((term) => evidenceTerms.includes(term));
    if (matched.length) {
      const reason = in_results === undefined ? 'matching_terms' : in_results && usesContext ? 'excluded_after_ranking' : 'below_evaluation_cutoff';
      record('other', { reason, matched_terms: matched });
    }
    else record('unresolved', { reason: 'no_term_alignment' });
  }
  return { report_id: report.id, build_id: report.build_id,
    counts: Object.fromEntries(Object.entries(classes).map(([key, questions]) => [key, questions.size])),
    candidates: [...candidates.values()], findings,
    ...(findings.some(({ reason }) => reason === 'legacy_mixed_recovery') && { hint: 'Run knowledge-nest eval again for per-evidence diagnostics.' }) };
}

export function dictionaryText(data) {
  const { counts, candidates, findings } = data;
  const displayTokens = (tokens) => tokens.map(({ surface, term }) => term === surface ? surface
    : `${surface} (${term ?? 'ignored'})`).join(' / ');
  return [
    `評価レポート ${data.report_id}`,
    `辞書候補 ${counts.dictionary}件 · 部分一致 ${counts.partial_match}件 · 辞書対象外 ${counts.other}件 · 未解決 ${counts.unresolved}件`,
    ...candidates.flatMap((candidate) => [candidate.csv, ...candidate.examples.flatMap((entry) => [
      `  ${entry.doc} — ${entry.quote}`,
      `  質問の分かち書き ${displayTokens(entry.question_tokens)}`,
      `  根拠の分かち書き ${displayTokens(entry.evidence_tokens)}`,
      `  語全体の質問 ${entry.queries.whole}`,
      `  部分語の質問 ${entry.queries.partial}`,
    ])]),
    ...findings.map(({ classification, reason, doc, question }) =>
      `  ${classification === 'other' ? '辞書対象外' : '未解決'} (${reason}) — ${question} [${doc}]`),
    ...(data.hint ? [data.hint] : []),
  ].join('\n');
}

export async function addDictionary(project, configPath, selected) {
  if (await realpath(configPath) === await realpath(project.configPath)) {
    throw new DictionaryError(2, 'CANDIDATE_REQUIRED', 'Use --config with a separate candidate configuration.');
  }
  const keyword = project.plan.index.find(({ module }) => module.name === 'index.keyword');
  if (keyword?.args.tokenizer !== 'lindera') {
    throw new DictionaryError(2, 'LINDERA_REQUIRED', 'Set the candidate keyword tokenizer to lindera.');
  }
  const input = await readFile(configPath, 'utf8');
  if (revision(input) !== project.configDigest) {
    throw new DictionaryError(4, 'CANDIDATE_CHANGED', 'The candidate configuration changed. Retry with its latest contents.');
  }
  const csv = await KnowledgeNest.mergeUserDictionary(keyword.args.user_dictionary ?? undefined, resolve(selected)).catch((error) => {
    if (error.code === 'INVALID_ARGUMENT') throw new DictionaryError(2, 'INVALID_USER_DICTIONARY',
      `Check the selected CSV ${selected}${keyword.args.user_dictionary ? ' and existing CSV ' + keyword.args.user_dictionary : ''}. ${error.message}`);
    throw error;
  });
  const file = 'user-dictionary-' + revision(csv) + '.csv';
  const path = join(dirname(configPath), file);
  const current = await readFile(path, 'utf8').catch((error) => {
    if (error.code === 'ENOENT') return null;
    throw error;
  });
  if (current !== null && current !== csv) throw new DictionaryError(2, 'DICTIONARY_EXISTS', 'Another dictionary already exists at ' + path);
  if (current === null) await atomicWrite(path, csv);
  if (await readFile(configPath, 'utf8') !== input) {
    throw new DictionaryError(4, 'CANDIDATE_CHANGED', 'The candidate configuration changed. Retry with its latest contents.');
  }
  const yaml = parseDocument(input);
  const index = project.config.index.modules.findIndex(({ use }) => use === 'index.keyword');
  const key = ['index', 'modules', index, 'with', 'user_dictionary'];
  const dictionary = './' + file;
  if (yaml.getIn(key) !== dictionary) {
    yaml.setIn(key, dictionary);
    await atomicWrite(configPath, yaml.toString());
  }
  return { config: configPath, user_dictionary: path };
}
