import { z } from 'zod';
import { revision } from './files.mjs';

const side = z.object({ item: z.number().int().positive(), quote: z.string().min(1) }).strict();
const findings = z.object({ issues: z.array(z.object({
  kind: z.enum(['contradiction', 'source_disagreement', 'duplicate']),
  first: side, second: side, description: z.string().min(1),
}).strict()) }).strict();
const suggested = z.object({ concepts: z.array(z.object({ name: z.string().min(1), pages: z.array(z.number().int().positive()) }).strict()),
  questions: z.array(z.string()), sources: z.array(z.string()) }).strict();
const segmenter = new Intl.Segmenter('ja', { granularity: 'word' });

export function ensureWikiInspection(db) {
  db.exec(`CREATE TABLE IF NOT EXISTS wiki_reviewed_material (id TEXT PRIMARY KEY, fingerprint TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS wiki_content_pairs (id TEXT PRIMARY KEY, data TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS wiki_suggestions (id TEXT PRIMARY KEY, data TEXT NOT NULL)`);
}

/** The concepts that pages name but that have no page, and what the model suggests to ask and to read next. */
export async function suggestWikiContent(db, pages, about, generate) {
  const active = pages.filter((page) => !page.archived);
  if (!active.length) return { issues: [], suggestions: { questions: [], sources: [] } };
  const input = { about, pages: active.map(({ id, title, markdown }, index) => ({ item: index + 1, id, title, text: markdown.slice(0, 600) })) };
  const fingerprint = revision(JSON.stringify(input));
  const saved = db.prepare("SELECT data FROM wiki_suggestions WHERE id = 'latest'").get();
  const record = saved && JSON.parse(saved.data);
  if (record?.fingerprint === fingerprint) return { issues: record.issues, suggestions: record.suggestions };
  const { concepts, questions, sources } = await generate(suggested,
    'Read the list of pages of the wiki. Name the important concepts that the pages mention but that have no page of their own, with the numbers of the pages that mention them. Suggest questions worth investigating next, and sources worth looking for to fill the gaps of the wiki. Write in the language of the project.', input);
  const folded = (text) => text.normalize('NFKC').toLowerCase();
  const lines = (list) => list.map((line) => line.trim()).filter(Boolean).slice(0, 10);
  const issues = [];
  const seen = new Set();
  for (const { name, pages: numbers } of concepts) {
    const key = folded(name.trim());
    if ([...key].length < 2 || seen.has(key) || active.some(({ id, title }) => folded(title.trim()) === key || folded(id.split('/').at(-1)) === key)) continue;
    const mentioned = [...new Set(numbers.map((number) => active[number - 1]).filter((page) => page && folded(page.markdown).includes(key)).map(({ id }) => id))];
    if (!mentioned.length) continue;
    seen.add(key);
    issues.push({ code: 'MISSING_PAGE', concept: name.trim(), pages: mentioned,
      description: '「' + name.trim() + '」は ' + mentioned.join('、') + ' に出てきますが、ページがありません' });
  }
  const suggestions = { questions: lines(questions), sources: lines(sources) };
  db.prepare("INSERT OR REPLACE INTO wiki_suggestions VALUES ('latest', ?)").run(JSON.stringify({ fingerprint, issues, suggestions }));
  return { issues, suggestions };
}

function tokens(text) {
  return [...new Set([...segmenter.segment(text.slice(0, 1000).normalize('NFKC').toLowerCase())]
    .filter(({ isWordLike, segment }) => isWordLike && [...segment].length >= 2 && /\p{L}/u.test(segment))
    .map(({ segment }) => segment))].slice(0, 35);
}

function itemKeys(item) {
  return [...(item.type === 'page' ? item.value.citations.map(({ source }) => 'source:' + source.sourceId) : ['source:' + item.value.sourceId]),
    ...tokens(item.title + '\n' + item.text).map((token) => 'word:' + token)];
}

const entityId = (first, second) => JSON.stringify([first.id, second.id].sort());

function excerpt(item, quote) {
  const at = item.text.indexOf(quote);
  if (at === -1 || item.text.indexOf(quote, at + 1) !== -1) return null;
  if (item.type === 'page') {
    const paragraph = item.text.split('\n\n').find((value) => value.includes(quote));
    const markers = paragraph?.match(/(?: \[\d+\])+$/u)?.[0];
    const citations = markers && [...markers.matchAll(/\[(\d+)\]/gu)].map(([, marker]) => item.value.citations[Number(marker) - 1]).filter(Boolean);
    return citations?.length ? { page: item.value.id, quote, citations } : null;
  }
  const prefix = item.text.slice(0, at);
  const startLine = 1 + (prefix.match(/\n/gu)?.length ?? 0);
  const endLine = startLine + quote.replace(/\n$/u, '').split('\n').length - 1;
  return { source: item.value, quote, citations: [{ ...item.evidence, range: { startLine, endLine },
    byteRange: { start: Buffer.byteLength(prefix), end: Buffer.byteLength(prefix + quote) } }] };
}

async function getMaterial(db, database, pages) {
  const material = new Map();
  for (const page of pages.filter(({ archived }) => !archived)) {
    const id = 'page:' + page.id;
    material.set(id, { id, type: 'page', value: page, title: page.title, text: page.markdown.slice(0, 4500),
      fingerprint: revision(JSON.stringify([page.title, page.markdown, page.citations.map(({ source, byteRange }) => [source.version, byteRange])])) });
  }
  for (const { data } of db.prepare('SELECT data FROM wiki_sources').iterate()) {
    const source = JSON.parse(data);
    const document = await database.getDocument(source);
    if (!document || document.evidence.source.version !== source.version) continue;
    const id = 'source:' + source.sourceId;
    material.set(id, { id, type: 'source', value: source, title: source.objectId, text: document.content.slice(0, 4500),
      evidence: document.evidence, fingerprint: source.version });
  }
  return material;
}

export async function inspectWikiContent(db, database, pages, { page, generate, progress }) {
  const material = await getMaterial(db, database, pages);
  const getReviewed = db.prepare('SELECT fingerprint FROM wiki_reviewed_material WHERE id = ?');
  const markReviewed = db.prepare('INSERT OR REPLACE INTO wiki_reviewed_material VALUES (?, ?)');
  const findPair = db.prepare('SELECT data FROM wiki_content_pairs WHERE id = ?');
  const savePair = db.prepare('INSERT OR REPLACE INTO wiki_content_pairs VALUES (?, ?)');
  const anchors = [...material.values()].filter((item) => page ? item.id === 'page:' + page
    : getReviewed.get(item.id)?.fingerprint !== item.fingerprint);
  const postings = new Map();
  for (const item of material.values()) for (const key of itemKeys(item)) {
    const list = postings.get(key) ?? [];
    list.push(item);
    postings.set(key, list);
  }
  let batches = 0;
  let pairs = 0;
  for (const [completed, anchor] of anchors.entries()) {
    progress('reviewing_content', { completed, total: anchors.length });
    const similarity = new Map();
    for (const key of new Set(itemKeys(anchor))) {
      const candidates = postings.get(key);
      if (candidates.length > 40 && !key.startsWith('source:')) continue;
      for (const other of candidates) {
        if (other === anchor || anchor.type === 'source' && other.type === 'source') continue;
        const relevance = key.startsWith('source:') ? 100 : 1 / candidates.length;
        similarity.set(other, (similarity.get(other) ?? 0) + relevance);
      }
    }
    const related = [...similarity].sort((a, b) => b[1] - a[1] || a[0].id.localeCompare(b[0].id))
      .slice(0, 6).map(([item]) => item);
    const unreviewed = related.filter((item) => {
      const record = findPair.get(entityId(anchor, item));
      if (!record) return true;
      const pair = JSON.parse(record.data);
      return pair.versions[anchor.id] !== anchor.fingerprint || pair.versions[item.id] !== item.fingerprint;
    });
    if (unreviewed.length) {
      const options = [anchor, ...unreviewed];
      const { issues } = await generate(findings,
        'Assess the numbered excerpts for factual contradictions, disagreement between a source and a page, or duplicate page content. ' +
        'Every comparison must include item 1. Use two verbatim quotes, one from each excerpt. Return an empty list where there is no issue.', {
          items: options.map(({ type, title, text }, index) => ({ item: index + 1, type, title, text })),
        });
      const byOther = unreviewed.map(() => []);
      for (const { kind, first, second, description } of issues) {
        if (first.item !== 1 && second.item !== 1 || first.item === second.item) continue;
        const a = options[first.item - 1];
        const b = options[second.item - 1];
        if (!a || !b || (kind === 'source_disagreement') !== (a.type !== b.type)) continue;
        const ordered = [[a, first], [b, second]].sort((x, y) => x[0].id.localeCompare(y[0].id));
        const evidence = ordered.map(([item, ref]) => excerpt(item, ref.quote));
        if (evidence.some((item) => !item)) continue;
        const id = 'issue-' + revision(JSON.stringify([kind, ordered.map(([item, ref]) => [item.id, ref.quote.normalize('NFKC')])]));
        byOther[Math.max(first.item, second.item) - 2].push({ id, code: 'CONTENT_CANDIDATE', kind, description,
          pages: ordered.filter(([item]) => item.type === 'page').map(([item]) => item.value.id), evidence });
      }
      for (const [i, item] of unreviewed.entries()) {
        savePair.run(entityId(anchor, item), JSON.stringify({ versions: { [anchor.id]: anchor.fingerprint, [item.id]: item.fingerprint },
          issues: byOther[i] }));
      }
      batches++;
      pairs += unreviewed.length;
    }
    if (!page) markReviewed.run(anchor.id, anchor.fingerprint);
  }
  progress('reviewing_content', { completed: anchors.length, total: anchors.length });
  const issues = [];
  for (const { data } of db.prepare('SELECT data FROM wiki_content_pairs').iterate()) {
    const record = JSON.parse(data);
    if (Object.entries(record.versions).every(([id, fingerprint]) => material.get(id)?.fingerprint === fingerprint)) issues.push(...record.issues);
  }
  return { issues, review: { changed_items: anchors.length, compared_pairs: pairs, model_calls: batches } };
}
