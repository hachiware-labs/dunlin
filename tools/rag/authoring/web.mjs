import { once } from 'node:events';
import { createServer } from 'node:http';

import { categories, citationLocations, idParts } from './files.mjs';
import { contentSecurityPolicy, escapeHtml, link, renderDocument, renderMarkdown } from './html.mjs';
import { paraContext } from './para.mjs';
import { readCorpusLines } from './sources.mjs';
import { isUploadedSource, originalMimeType, readOriginal } from './uploads.mjs';
import { getWiki } from './wiki.mjs';

const host = '127.0.0.1';
const sectionTitles = { para: 'PARA', wiki: 'LLM Wiki' };
const digestPattern = /^[a-f0-9]{64}$/u;

export async function startAuthoringWeb(database, { paraRoot, wikiRoot, uploadRoot }, port) {
  const sections = {};
  if (paraRoot) sections.para = (id) => paraContext(paraRoot, id);
  if (wikiRoot) sections.wiki = (id) => getWiki(wikiRoot, id);
  const collections = Object.keys(sections).map((section) => link(`/${section}`, sectionTitles[section])).join(' ');
  const navigation = `<nav aria-label="コレクション"><a href="/">Dunlin</a> ${collections}</nav>\n`;
  const server = createServer(async (request, response) => {
    response.setHeader('Cache-Control', 'no-store');
    response.setHeader('X-Content-Type-Options', 'nosniff');
    response.setHeader('Content-Security-Policy', `${contentSecurityPolicy}; frame-ancestors 'none'`);
    const page = (status, title, body = '') => {
      response.writeHead(status, { 'Content-Type': 'text/html; charset=utf-8' });
      response.end(request.method === 'HEAD' ? undefined : renderDocument(title, body, { navigation }));
    };

    try {
      const actualPort = server.address().port;
      const allowedHosts = actualPort === 80 ? [host, 'localhost', `${host}:80`, 'localhost:80'] : [`${host}:${actualPort}`, `localhost:${actualPort}`];
      if (!allowedHosts.includes(request.headers.host?.toLowerCase())) return page(403, 'アクセスできません');
      if (request.method !== 'GET' && request.method !== 'HEAD') {
        response.setHeader('Allow', 'GET, HEAD');
        return page(405, '閲覧専用です');
      }
      const [pathname] = request.url.split('?');
      if (!pathname.startsWith('/') || pathname.startsWith('//') || pathname.includes('\\')) return page(400, '無効な URL');
      const url = new URL(request.url, `http://${host}`);
      const [, section, ...segments] = pathname.split('/');
      if (!section) return page(200, 'Dunlin', `<p>閲覧するコレクションを選んでください。</p><p>${collections}</p>`);
      if (section === 'original') {
        const [sourceId, digest] = segments;
        if (segments.length !== 2 || !isUploadedSource(sourceId) || !digestPattern.test(digest)) return page(404, '原本が見つかりません');
        const bytes = await readOriginal(uploadRoot, sourceId, digest).catch((error) => {
          if (error.message !== 'Original file not found') throw error;
          return null;
        });
        if (!bytes) return page(404, '原本が見つかりません');
        response.writeHead(200, {
          'Content-Type': originalMimeType, 'Content-Disposition': 'attachment', 'Content-Length': bytes.length,
        });
        return response.end(request.method === 'HEAD' ? undefined : bytes);
      }

      if (!Object.hasOwn(sections, section)) return page(404, 'ページが見つかりません');
      const load = sections[section];
      if (!segments.length || (segments.length === 1 && !segments[0])) {
        return page(200, sectionTitles[section], renderIndex(section, await load()));
      }
      const id = decodePageId(segments);
      if (!id || (section === 'para' && (!categories.includes(id.split('/')[0]) || !id.includes('/')))) {
        return page(404, 'ページが見つかりません');
      }
      const result = await load(id);
      const article = result.note ?? result.page;
      if (!article) return page(404, 'ページが見つかりません');
      const articlePath = pagePath(section, id);
      if (!url.searchParams.has('citation')) {
        return page(200, article.title, `<article>${renderMarkdown(article.markdown)}</article>${renderCitations(article, articlePath)}`);
      }
      const index = url.searchParams.get('citation');
      const citation = /^[1-9]\d?$/u.test(index) ? article.citations[Number(index) - 1] : null;
      const back = `<p>${link(articlePath, 'ページに戻る')}</p>`;
      if (url.searchParams.get('rev') !== article.revision) return page(409, 'ページが更新されました', back);
      if (!citation) return page(404, '出典が見つかりません', back);
      if (url.searchParams.has('page')) {
        const pageNumber = Number(url.searchParams.get('page'));
        const locations = citation.locations?.filter((location) => location.page === pageNumber);
        const original = originalParts(citation.original);
        if (!original || !locations?.length) return page(404, '原本のページが見つかりません', back);
        const bytes = await readOriginal(uploadRoot, ...original);
        const { previewPage } = await import('../parsers/pages.mjs');
        const png = await previewPage(bytes, pageNumber, locations);
        response.writeHead(200, { 'Content-Type': 'image/png', 'Content-Length': png.length });
        return response.end(request.method === 'HEAD' ? undefined : png);
      }
      const citationPath = `${articlePath}?citation=${index}&rev=${article.revision}`;
      return page(200, citation.source.objectId, `${back}${await renderExcerpt(database, citation, citationPath)}`);
    } catch (error) {
      console.error(`Dunlin Web: ${error.message}`);
      return page(500, 'ページを読み込めません');
    }
  });
  server.listen(port, host);
  await once(server, 'listening');
  return server;
}

function decodePageId(segments) {
  try {
    const decoded = segments.map(decodeURIComponent);
    const id = decoded.join('/');
    return idParts(id).length === decoded.length ? id : null;
  } catch {
    return null;
  }
}

function pagePath(section, id) {
  return `/${section}/${id.split('/').map(encodeURIComponent).join('/')}`;
}

function renderIndex(section, result) {
  if (section === 'wiki') {
    return renderList(result.pages.map(({ id, title }) => link(pagePath(section, id), title)));
  }
  return categories.map((category) => {
    const notes = result.notes.filter((id) => id.startsWith(`${category}/`));
    return `<section><h2>${category}</h2>${renderList(notes.map((id) => link(pagePath(section, id), id.slice(category.length + 1))))}</section>`;
  }).join('');
}

function renderList(items) {
  return items.length ? `<ul>${items.map((item) => `<li>${item}</li>`).join('')}</ul>` : '<p>公開済みの項目はありません。</p>';
}

function renderCitations({ revision, citations }, articlePath) {
  if (!citations.length) return '';
  const links = citations.map((citation, index) => {
    const url = `${articlePath}?citation=${index + 1}&rev=${encodeURIComponent(revision)}`;
    return `${link(url, citation.source.objectId)} · ${lineRange(citation)} ${sourceLinks(citation)}`;
  });
  return `<section><h2>出典</h2>${renderList(links)}</section>`;
}

async function renderExcerpt(database, citation, citationPath) {
  const corpus = await database.getCorpusDocument(citation.source, citation.source.version);
  let excerpt;
  if (corpus) {
    excerpt = readCorpusLines(corpus, citation);
  } else {
    // Older publications can still refer to the current index.
    await database.getIndexRevision();
    excerpt = await database.read(citation).catch((error) => {
      if (error.code === 'STALE_EVIDENCE' || error.code === 'NOT_FOUND') return null;
      throw error;
    });
  }
  const content = excerpt ? `<pre>${escapeHtml(excerpt.content)}</pre>` : '<p>引用時の本文を取得できません。</p>';
  const pages = originalParts(citation.original) ? [...new Set(citation.locations?.map(({ page }) => page).filter(Boolean))] : [];
  const previews = pages.map((page) => `<figure><figcaption>原本 p. ${page}</figcaption><img loading="lazy" src="${escapeHtml(`${citationPath}&page=${page}`)}" alt="原本 ${page} ページの出典箇所"></figure>`);
  return `<p>${lineRange(citation)} ${sourceLinks(citation)}</p>${content}${previews.join('')}`;
}

function lineRange({ range, locations }) {
  const position = citationLocations(locations);
  return escapeHtml(`L${range.startLine}–L${range.endLine}${position ? ` · ${position}` : ''}`);
}

function sourceLinks({ source, original }) {
  const links = [];
  const url = URL.parse(source.url);
  if (url && ['http:', 'https:'].includes(url.protocol)) links.push(link(url.href, '出典 URL'));
  const parts = originalParts(original);
  if (parts) {
    const path = `/original/${parts.join('/')}`;
    links.push(`<a href="${path}" download="${escapeHtml(original.name)}">原本を取得</a>`);
  }
  return links.join(' · ');
}

function originalParts(original) {
  return /^dunlin:\/\/original\/(upload-[a-f0-9]{57})\/([a-f0-9]{64})$/u.exec(original?.uri)?.slice(1);
}

