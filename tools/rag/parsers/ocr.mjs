import { createRequire } from 'node:module';

import { lightonModel } from './catalog.mjs';
import { acquireLighton } from './lighton-process.mjs';
import { managedOcr, probeGpu, setupManagedOcr } from './lighton-setup.mjs';
import { imageCanvas, pageDependencies, pdfBytes, pdfCanvas, pdfPageText, withPdf } from './pages.mjs';
import { fetchService, ServiceUnavailable } from './service-request.mjs';

const require = createRequire(import.meta.url);
const langPacks = { ja: ['jpn', '@tesseract.js-data/jpn'], 'ja-vert': ['jpn_vert', '@tesseract.js-data/jpn_vert'] };

export async function check(options, setup, { backend } = {}) {
  await pageDependencies();
  if (options.tool === 'tesseract-js') {
    const worker = await tesseractWorker(options.lang);
    await worker.terminate();
    return { lang: options.lang };
  }
  if (!options.managed) {
    try {
      return await checkService(options);
    } catch (error) {
      if (!canUseLocal(options, error)) throw error;
      return tryLocal(error, async () => ({ ...(await checkLocal(options)), serving: false }));
    }
  }
  return checkLocal(options, setup, backend);
}

async function checkLocal(options, setup, backend) {
  const info = setup ? await setupManagedOcr(backend) : await managedOcr().then(({ python, backend }) => probeGpu(python, backend));
  return { ...info, lang: options.lang, model: options.model, prepared: true, execution: 'local' };
}

async function checkService(options) {
  const result = await lightonRequest(options, 'models');
  const model = result.data?.find(({ id }) => id === options.model);
  if (!model) throw new Error(`LightOnOCR service does not advertise ${options.model}`);
  const status = { model: options.model, lang: options.lang, serving: true, execution: 'shared' };
  for (const field of ['device', 'deviceName', 'runtime', 'dtype']) {
    if (typeof model[field] === 'string') status[field] = model[field];
  }
  return status;
}

function canUseLocal(options, error) {
  return options.model === lightonModel && error instanceof ServiceUnavailable;
}

async function tryLocal(sharedError, run, signal) {
  try {
    return await run();
  } catch (error) {
    signal?.throwIfAborted();
    throw new Error(`${sharedError.message}. Local GPU OCR failed: ${error.message}`, { cause: error });
  }
}

export async function parseOcr(bytes, options, signal) {
  let worker;
  let local;
  const lines = [];
  const locations = [];
  async function recognizeLocal(bytes) {
    local ??= await acquireLighton(signal);
    return recognizeLighton(bytes, { model: options.model, baseUrl: local.baseUrl }, signal);
  }
  async function recognizePage(canvas) {
    const bytes = canvas.toBuffer('image/png');
    if (options.tool === 'tesseract-js') {
      worker ??= await tesseractWorker(options.lang);
      return recognizeTesseract(bytes, canvas, worker);
    }
    if (options.managed || local) return recognizeLocal(bytes);
    try {
      return await recognizeLighton(bytes, options, signal);
    } catch (error) {
      if (!canUseLocal(options, error)) throw error;
      return tryLocal(error, () => recognizeLocal(bytes), signal);
    }
  }
  async function addPage(page, render, text = '') {
    signal?.throwIfAborted();
    const blocks = text.trim() ? [{ text }] : await recognizePage(await render());
    for (const block of blocks) {
      const body = block.text.trim();
      if (!body) continue;
      const startLine = lines.length + 1;
      lines.push(...body.split(/\r?\n/u));
      locations.push({ page, range: { startLine, endLine: lines.length }, ...(block.bbox ? { bbox: block.bbox } : {}) });
    }
  }
  try {
    if (pdfBytes(bytes)) {
      await withPdf(bytes, async (pdf, createCanvas) => {
        if (pdf.numPages > 128) throw new Error('OCR supports at most 128 pages per PDF');
        for (let i = 1; i <= pdf.numPages; i++) {
          signal?.throwIfAborted();
          const page = await pdf.getPage(i);
          const text = await pdfPageText(page);
          await addPage(i, () => pdfCanvas(page, createCanvas), text);
          page.cleanup();
        }
      });
    } else {
      await addPage(1, () => imageCanvas(bytes));
    }
    if (!lines.length) throw new Error('OCR did not find any document text');
    return { content: lines.join('\n'), format: 'text', locations };
  } finally {
    await local?.release();
    await worker?.terminate();
  }
}

async function recognizeTesseract(bytes, canvas, worker) {
  const { data } = await worker.recognize(bytes, {}, { blocks: true });
  return (data.blocks ?? []).flatMap((block) => block.paragraphs.flatMap((paragraph) => paragraph.lines))
    .map(({ text, bbox }) => ({ text, bbox: [bbox.x0, bbox.y0, bbox.x1, bbox.y1].map((value, index) =>
      Math.max(0, Math.min(1000, Math.round(1000 * value / (index % 2 ? canvas.height : canvas.width))))) }));
}

async function recognizeLighton(bytes, options, signal) {
  const result = await lightonRequest(options, 'chat/completions', {
    model: options.model,
    messages: [{ role: 'user', content: [
      { type: 'image_url', image_url: { url: `data:image/png;base64,${bytes.toString('base64')}` } },
      { type: 'text', text: 'grounding' },
    ] }],
    temperature: 0.1, max_tokens: 4096,
  }, signal);
  const choice = result.choices?.[0];
  if (choice?.finish_reason === 'length') throw new Error('LightOnOCR output was truncated');
  const content = choice?.message?.content;
  if (typeof content !== 'string') throw new Error('LightOnOCR returned no text');
  return groundedBlocks(content);
}

function groundedBlocks(content) {
  const markers = [...content.matchAll(/!\[([a-z_+]+)\]\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*\)/gu)];
  if (!markers.length && content.trim()) throw new Error('LightOnOCR returned text without grounding coordinates');
  return markers.flatMap((marker, index) => {
    const [, label, ...coords] = marker;
    if (['page_number', 'image'].includes(label)) return [];
    const bbox = coords.map(Number);
    const [left, top, right, bottom] = bbox;
    if (right > 1000 || bottom > 1000 || left >= right || top >= bottom) throw new Error('LightOnOCR returned invalid grounding coordinates');
    const text = content.slice(marker.index + marker[0].length, markers[index + 1]?.index);
    return [{ text, bbox }];
  });
}

async function lightonRequest({ baseUrl, apiKey }, route, body, signal) {
  const response = await fetchService('LightOnOCR', baseUrl, route, {
    method: body ? 'POST' : 'GET',
    headers: { ...(apiKey ? { Authorization: `Bearer ${apiKey}` } : {}), ...(body ? { 'Content-Type': 'application/json' } : {}) },
    body: body ? JSON.stringify(body) : undefined,
  }, signal);
  if (!response.ok) {
    await response.body?.cancel();
    throw new Error(`LightOnOCR ${route} returned HTTP ${response.status}`);
  }
  return response.json();
}

async function tesseractWorker(lang) {
  if (!Object.hasOwn(langPacks, lang)) throw new Error('Tesseract OCR supports --lang ja or ja-vert in this setup');
  const [language, packageName] = langPacks[lang];
  try {
    const { createWorker } = await import('tesseract.js');
    const { langPath, gzip } = require(packageName);
    return await createWorker(language, 1, { langPath, gzip, cacheMethod: 'none' });
  } catch (error) {
    throw new Error(`Could not load ${packageName} with tesseract.js. Run npm ci in tools/rag. ${error.message}`, { cause: error });
  }
}
