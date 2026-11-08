import { once } from 'node:events';
import { createServer } from 'node:http';

import { localDocling } from './documents.mjs';

const host = '127.0.0.1';
const maxFileBytes = 16 * 1024 * 1024;
const maxBodyBytes = maxFileBytes + 64 * 1024;

export async function startDoclingService({ modelDir, port }) {
  const convert = await localDocling(modelDir, true);
  const server = createServer({ headersTimeout: 10_000, requestTimeout: 30_000 }, async (request, response) => {
    const send = (status, body) => {
      response.writeHead(status, { 'Content-Type': 'application/json; charset=utf-8', 'Cache-Control': 'no-store' });
      response.end(JSON.stringify(body));
    };
    const boundPort = server.address().port;
    const suffix = boundPort === 80 ? '' : `:${boundPort}`;
    const allowedHosts = [`${host}${suffix}`, `localhost${suffix}`, `${host}:${boundPort}`, `localhost:${boundPort}`];
    if (!allowedHosts.includes(request.headers.host?.toLowerCase())
      || (request.headers.origin && !allowedHosts.some((value) => request.headers.origin === `http://${value}`))) {
      return send(403, { error: 'Expected a local Docling client' });
    }
    if (request.method === 'GET' && request.url === '/ready') return send(200, { status: 'ready' });
    if (request.method !== 'POST' || request.url !== '/v1/convert/file') return send(404, { error: 'Unknown Docling endpoint' });

    let file;
    try {
      file = await readFilePart(request);
    } catch (error) {
      if (response.destroyed) return;
      response.setHeader('Connection', 'close');
      return send(error.status ?? 400, { error: error.message });
    }
    try {
      const { content, locations } = await convert(Buffer.from(await file.arrayBuffer()), file.name);
      if (Buffer.byteLength(content) > 1024 * 1024) return send(422, { error: 'Extracted text exceeds 1 MiB' });
      send(200, { status: 'success', document: { filename: file.name, md_content: content, dunlin_locations: locations } });
    } catch (error) {
      if (!response.destroyed) send(422, { status: 'failure', error: error.message });
    }
  });
  server.listen(port, host);
  await once(server, 'listening');
  return server;
}

async function readFilePart(request) {
  const contentType = request.headers['content-type'] ?? '';
  if (!/^multipart\/form-data\s*;/iu.test(contentType)) throw new Error('Expected multipart/form-data');
  if (Number(request.headers['content-length']) > maxBodyBytes) throw tooLarge();
  const chunks = [];
  let size = 0;
  for await (const chunk of request.iterator({ destroyOnReturn: false })) {
    size += chunk.length;
    if (size > maxBodyBytes) throw tooLarge();
    chunks.push(chunk);
  }
  const form = await new Response(Buffer.concat(chunks, size), { headers: { 'Content-Type': contentType } }).formData();
  const [file, ...extra] = form.getAll('files');
  if (!(file instanceof File) || extra.length || !/\.(?:pdf|docx|xlsx|pptx)$/iu.test(file.name)) {
    throw new Error('Expected one PDF, DOCX, XLSX or PPTX file');
  }
  if (file.size > maxFileBytes) throw tooLarge();
  for (const [name, value] of [['to_formats', 'md'], ['do_ocr', 'false'], ['image_export_mode', 'placeholder']]) {
    if (form.has(name) && form.get(name) !== value) throw new Error(`Only ${name}=${value} is supported`);
  }
  return file;
}

function tooLarge() {
  return Object.assign(new Error('Source file exceeds 16 MiB'), { status: 413 });
}
