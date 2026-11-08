import MarkdownIt from 'markdown-it';

import { citationLocations } from './files.mjs';

const markdown = new MarkdownIt({ html: false });
const publicationMetadata = /\n<!-- dunlin:publication ([\w-]+) -->\r?\n?$/u;
const sourceProtocols = new Set(['http:', 'https:', 'file:', 's3:', 'dunlin:']);
export const contentSecurityPolicy = "default-src 'none'; style-src 'unsafe-inline'; img-src 'self' data:; base-uri 'none'; form-action 'none'";
export const escapeHtml = markdown.utils.escapeHtml;

export function renderMarkdown(content) {
  return markdown.render(content);
}

export function link(href, label) {
  return `<a href="${escapeHtml(href)}">${escapeHtml(label)}</a>`;
}

export function renderDocument(title, body, { metadata, navigation = '' } = {}) {
  const safeTitle = escapeHtml(title);
  const footer = metadata ? `<!-- dunlin:publication ${Buffer.from(JSON.stringify(metadata)).toString('base64url')} -->\n` : '';
  return `<!doctype html>
<html lang="ja"><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="${contentSecurityPolicy}">
<title>${safeTitle} | Dunlin</title>
<style>
  body { font: 1rem/1.65 system-ui, sans-serif; max-width: 54rem; margin: auto; padding: 1.5rem; color-scheme: light dark; }
  nav, section { border-block-end: 1px solid GrayText; padding-block: .65rem; }
  nav a + a { margin-inline-start: 1.25rem; }
  article { overflow-wrap: anywhere; }
  pre { overflow: auto; padding: .85rem; border: 1px solid GrayText; border-radius: .35rem; }
  img { max-width: 100%; height: auto; }
</style>
${navigation}<main><h1>${safeTitle}</h1>${body}</main>
</html>\n${footer}`;
}

export function renderPublication({ title, markdown: content, citations }, metadata) {
  const entries = citations.map(({ source, range, original, locations }) => {
    const useOriginal = original && source.url.startsWith('dunlin://');
    const url = useOriginal ? original.uri : source.url;
    const sourceLink = sourceProtocols.has(URL.parse(url)?.protocol) ? link(url, source.objectId) : escapeHtml(source.objectId);
    const position = citationLocations(locations);
    const originalLink = original && !useOriginal ? ` · ${link(original.uri, '原本')}` : '';
    return `<li>${sourceLink} · L${range.startLine}–L${range.endLine}${position ? ` · ${escapeHtml(position)}` : ''}${originalLink}</li>`;
  });
  const sources = entries.length ? `<section><h2>出典</h2><ol>${entries.join('\n')}</ol></section>` : '';
  return renderDocument(title, `<article>${renderMarkdown(content)}</article>${sources}`, { metadata });
}

export function readPublication(content) {
  const match = publicationMetadata.exec(content);
  if (!match) throw new Error('Dunlin publication metadata is missing');
  return JSON.parse(Buffer.from(match[1], 'base64url').toString('utf8'));
}
