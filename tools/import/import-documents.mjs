import { extname } from 'node:path/posix';
import { resolve } from 'node:path';

import { GetObjectCommand, ListObjectsV2Command, S3Client } from '@aws-sdk/client-s3';
import { load } from 'cheerio';
import { Dunlin } from 'dunlin';
import { prepareDocument } from '../rag/authoring/uploads.mjs';

const documentLimit = 1024 * 1024;
const htmlLimit = 4 * documentLimit;
const batchLimit = 64 * documentLimit;
const requestTimeoutMs = 30_000;
const extensions = new Map([
  ['.txt', 'text'], ['.md', 'markdown'], ['.markdown', 'markdown'], ['.html', 'html'], ['.htm', 'html'],
]);

export async function importDocuments(indexPath, sourceId, locations, s3Endpoint, { corpusBackend, keepOriginal, decode, supports } = {}) {
  const documents = new Map();
  const uploadRoot = resolve(indexPath, '.dunlin-uploads-v1');
  const decodeDocument = decode ?? ((body, type, name) => readDocument(body, type, name, { keepOriginal }));
  let batchBytes = 0;
  let s3;

  async function collect(objectId, url, read) {
    if (documents.has(objectId)) return;
    try {
      const document = await read();
      batchBytes += Buffer.byteLength(document.content);
      if (batchBytes > batchLimit) throw new Error('The imported text exceeds 64 MiB');
      const { input } = await prepareDocument(uploadRoot, sourceId, { objectId, url }, document);
      documents.set(objectId, input);
    } catch (error) {
      throw new Error(`Could not import ${url}: ${error.message}`, { cause: error });
    }
  }

  try {
    for (const location of locations) {
      if (location.startsWith('s3://')) {
        const { bucket, key } = parseS3Location(location);
        s3 ??= new S3Client({
          ...(s3Endpoint && { endpoint: s3Endpoint, forcePathStyle: true }),
          requestHandler: { requestTimeout: requestTimeoutMs, throwOnRequestTimeout: true },
        });
        if (!key || key.endsWith('/')) {
          for await (const object of listDocuments(s3, bucket, key, supports)) {
            const url = s3Url(bucket, object.key);
            await collect(url, url, () => readS3(s3, bucket, object.key, decodeDocument, object.etag));
          }
        } else {
          await collect(s3Url(bucket, key), location, () => readS3(s3, bucket, key, decodeDocument));
        }
      } else {
        const url = new URL(location);
        if (!['http:', 'https:'].includes(url.protocol)) {
          throw new Error(`Expected an HTTP(S) or s3:// URL: ${location}`);
        }
        await collect(url.href, location, () => readHttpDocument(url, undefined, decodeDocument));
      }
    }
  } finally {
    s3?.destroy();
  }

  if (documents.size === 0) throw new Error('No supported documents found');
  const database = await Dunlin.open(indexPath, { corpusBackend });
  try {
    return await database.replaceDocuments(sourceId, [...documents.values()]);
  } finally {
    await database.close();
  }
}

function parseS3Location(location) {
  const match = /^s3:\/\/([a-z0-9][a-z0-9.-]*)(?:\/([^?#]*))?$/u.exec(location);
  if (!match || /\s/u.test(location)) throw new Error(`Invalid S3 URL: ${location}`);
  return { bucket: match[1], key: decodeURIComponent(match[2] ?? '') };
}

function s3Url(bucket, key) {
  return `s3://${bucket}/${key.split('/').map(encodeURIComponent).join('/')}`;
}

async function* listDocuments(client, bucket, prefix, supports) {
  let token;
  do {
    const page = await client.send(new ListObjectsV2Command({
      Bucket: bucket, Prefix: prefix, EncodingType: 'url', ContinuationToken: token,
    }));
    for (const object of page.Contents ?? []) {
      if (object.Key === undefined) throw new Error('S3 listing returned an object without a key');
      const key = page.EncodingType === 'url' ? decodeURIComponent(object.Key) : object.Key;
      if (extensions.has(extname(key).toLowerCase()) || supports?.(key)) yield { key, etag: object.ETag };
    }
    if (!page.IsTruncated) return;
    if (!page.NextContinuationToken || page.NextContinuationToken === token) {
      throw new Error(`S3 listing did not advance for ${s3Url(bucket, prefix)}`);
    }
    token = page.NextContinuationToken;
  } while (token);
}

async function readS3(client, bucket, key, decode, etag) {
  const result = await client.send(new GetObjectCommand({ Bucket: bucket, Key: key, IfMatch: etag }));
  try {
    return await decode(result.Body, result.ContentType, key);
  } finally {
    result.Body?.destroy();
  }
}

export async function readHttpDocument(url, signal, decode = readDocument) {
  const timeout = AbortSignal.timeout(requestTimeoutMs);
  const response = await fetch(url, { signal: signal ? AbortSignal.any([signal, timeout]) : timeout });
  try {
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    return await decode(response.body, response.headers.get('content-type'), new URL(response.url).pathname);
  } finally {
    await response.body?.cancel().catch(() => {});
  }
}

export async function readDocument(body, contentType, path, { keepOriginal } = {}) {
  const format = documentFormat(contentType, path);
  if (!body) throw new Error('Response has no document body');
  const limit = format === 'html' ? htmlLimit : documentLimit;
  const chunks = [];
  let size = 0;
  for await (const chunk of body) {
    size += chunk.byteLength;
    if (size > limit) throw new Error(`Response exceeds ${limit / documentLimit} MiB`);
    chunks.push(chunk);
  }
  const bytes = Buffer.concat(chunks, size);
  const raw = new TextDecoder('utf-8', { fatal: true }).decode(bytes);
  const content = format === 'html' ? extractHtml(raw) : raw;
  if (!content.trim()) throw new Error('Document has no text');
  if (Buffer.byteLength(content) > documentLimit) throw new Error('Extracted text exceeds 1 MiB');
  return { format: format === 'html' ? 'text' : format, content,
    ...(format === 'html' && { parser: { tool: 'cheerio' } }), ...(keepOriginal && { bytes }) };
}

function documentFormat(contentType, path) {
  const mime = contentType?.split(';')[0].trim().toLowerCase();
  const extension = extensions.get(extname(path).toLowerCase());
  if (mime === 'text/html' || mime === 'application/xhtml+xml') return 'html';
  if (mime === 'text/markdown' || mime === 'text/x-markdown') return 'markdown';
  if (mime === 'text/plain') return extension === 'markdown' ? 'markdown' : 'text';
  if (mime?.startsWith('text/')) return 'text';
  if ((!mime || mime === 'application/octet-stream') && extension) return extension;
  throw new Error(`Unsupported document type (${mime || 'unknown'})`);
}

function extractHtml(html) {
  const $ = load(html);
  $('script, style, noscript, template, nav, footer, aside, [hidden]').remove();
  const main = $('main, [role="main"]').first();
  const article = $('article').first();
  const body = main.length ? main : article.length ? article : $('body');
  body.find('h1, h2, h3, h4, h5, h6, p, li, blockquote, pre, div, section, tr, br').before('\n').after('\n');
  return body.text().split(/\r?\n/u).map((line) => line.trim()).filter(Boolean).join('\n');
}
