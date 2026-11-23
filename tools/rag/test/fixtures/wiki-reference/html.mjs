import MarkdownIt from 'markdown-it';

import { citationLocations } from './files.mjs';

const markdown = new MarkdownIt({ html: false });
const recordStart = '<script type="application/json" id="knowledge-nest-page">';
const sourceProtocols = new Set(['http:', 'https:', 'file:', 's3:', 'knowledge-nest:']);
export const contentSecurityPolicy = "default-src 'none'; style-src 'unsafe-inline'; img-src 'self' data:; base-uri 'none'; form-action 'none'";
export const escapeHtml = markdown.utils.escapeHtml;

export function renderMarkdown(content) {
  return markdown.render(content);
}

export function link(href, label) {
  return `<a href="${escapeHtml(href)}">${escapeHtml(label)}</a>`;
}

/** The way up to the contents of the folder, as far above the page as its ID is deep. */
function up(metadata) {
  const parts = typeof metadata?.id === 'string' && metadata.id ? metadata.id.split('/') : [];
  return parts.length ? `<nav class="up"><a href="${'../'.repeat(parts.length - 1)}index.html">目録</a>${parts.slice(0, -1).map((part) => `<span>${escapeHtml(part)}</span>`).join('')}</nav>` : '';
}

/** The sections of a page: each `h2` gets an id, and three or more of them a table of contents. */
function sectioned(html) {
  const entries = [];
  const text = html.replace(/<h2>(.*?)<\/h2>/gu, (_, inner) => {
    entries.push(inner.replace(/<[^>]+>| \[\d+\]/gu, ''));
    return `<h2 id="section-${entries.length}">${inner}</h2>`;
  });
  return [text, entries.length < 3 ? '' : `<nav class="toc" aria-label="目次"><h2>目次</h2><ol>${entries.map((entry, index) => `<li><a href="#section-${index + 1}">${entry}</a></li>`).join('')}</ol></nav>`];
}

export function renderDocument(title, body, { metadata, navigation = '' } = {}) {
  const safeTitle = escapeHtml(title);
  // The page the file holds, as JSON.
  const record = metadata ? `${recordStart}${JSON.stringify(metadata).replaceAll('<', '\\u003c')}</script>\n` : '';
  return `<!doctype html>
<html lang="ja"><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="${contentSecurityPolicy}">
<title>${safeTitle} | Knowledge-Nest</title>
<style>
  body { font: 1rem/1.65 system-ui, sans-serif; max-width: 54rem; margin: auto; padding: 1.5rem; color-scheme: light dark; }
  nav, section { border-block-end: 1px solid GrayText; padding-block: .65rem; }
  nav a + a { margin-inline-start: 1.25rem; }
  article { overflow-wrap: anywhere; }
  pre { overflow: auto; padding: .85rem; border: 1px solid GrayText; border-radius: .35rem; }
  img { max-width: 100%; height: auto; }
</style>
${navigation}<main>${up(metadata)}<h1>${safeTitle}</h1>${body}</main>
${record}</html>\n`;
}

/** The documents a page cites, in the order they are first cited: the indices of their citations. */
function citedDocuments(citations) {
  const documents = new Map();
  citations.forEach(({ source }, index) => {
    const key = source?.sourceId ?? source?.objectId ?? '';
    if (!documents.has(key)) documents.set(key, []);
    documents.get(key).push(index);
  });
  return [...documents.values()];
}

/** The source markers as the numbers of their documents, once after statements in a row that cite the same ones. */
function documentMarkers(html, citations) {
  const numberOf = [];
  citedDocuments(citations).forEach((list, at) => { for (const index of list) numberOf[index] = at + 1; });
  const found = [...html.matchAll(/(?: \[\d+\])+/gu)].map((group) => {
    const numbers = group[0].split(' [').slice(1).map((part) => Number(part.slice(0, -1)));
    const documents = numbers.every((number) => Number.isInteger(number) && number >= 1 && number <= citations.length)
      ? [...new Set(numbers.map((number) => numberOf[number - 1]))].sort((a, b) => a - b) : null;
    return { start: group.index, end: group.index + group[0].length, documents };
  });
  let out = '';
  let last = 0;
  found.forEach(({ start, end, documents }, at) => {
    out += html.slice(last, start);
    last = end;
    if (!documents) {
      out += html.slice(start, end);
      return;
    }
    const next = found[at + 1];
    const continued = next?.documents && next.documents.join() === documents.join() && !html.slice(end, next.start).includes('<h');
    if (!continued) out += documents.map((document) => `<a class="ref" href="#source-${document}">${document}</a>`).join('');
  });
  return out + html.slice(last);
}

export function renderPublication({ title, markdown: content, citations }, metadata) {
  const entries = citedDocuments(citations).map((list, index) => {
    const { source, original } = citations[list[0]];
    const useOriginal = original && source.url.startsWith('knowledge-nest://');
    const url = useOriginal ? original.uri : source.url;
    // A source in the project is a path from the page, which has no scheme.
    const linked = sourceProtocols.has(URL.parse(url)?.protocol) || (URL.parse(url) === null && url !== '');
    const sourceLink = linked ? link(url, source.objectId) : escapeHtml(source.objectId);
    const originalLink = original && !useOriginal ? ` · ${link(original.uri, '原本')}` : '';
    const lines = list.map((at) => {
      const { range, locations } = citations[at];
      const position = citationLocations(locations);
      return `L${range.startLine}–L${range.endLine}${position ? ` (${escapeHtml(position)})` : ''}`;
    }).filter((line, at, all) => all.indexOf(line) === at).join(', ');
    return `<li id="source-${index + 1}">${sourceLink} · ${lines}${originalLink}</li>`;
  });
  const sources = entries.length ? `<section class="notes"><h2>出典</h2><ol>${entries.join('\n')}</ol></section>` : '';
  const [text, contents] = sectioned(renderMarkdown(content));
  const cited = documentMarkers(text, citations);
  return renderDocument(title, `${contents}<article class="body">${cited}</article>${sources}`, { metadata });
}

export function readPublication(content) {
  const at = content.indexOf(recordStart);
  if (at < 0) throw new Error('Knowledge-Nest publication metadata is missing');
  const rest = content.slice(at + recordStart.length);
  return JSON.parse(rest.slice(0, rest.indexOf('</script>')));
}
