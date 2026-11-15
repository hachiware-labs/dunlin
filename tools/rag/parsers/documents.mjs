import { extname } from 'node:path';
import { configureDoclingModels, installDoclingModels } from './docling-models.mjs';
import { pageDependencies, pdfPageText, withPdf } from './pages.mjs';
import { fetchService, ServiceUnavailable } from './service-request.mjs';

async function nativeDocling() {
  try {
    return await import('docling.rs');
  } catch (error) {
    throw new Error(`docling.rs is unavailable on ${process.platform}/${process.arch}. Run npm ci in tools/rag or configure a reachable Docling Serve endpoint.`, { cause: error });
  }
}

export async function check(options, setup) {
  if (options.tool === 'docling-serve') {
    try {
      await doclingRequest(options, 'ready');
      return { execution: 'shared' };
    } catch (error) {
      if (!(error instanceof ServiceUnavailable)) throw error;
    }
  }
  const { convertAsync, checkDependencies } = await nativeDocling();
  const result = await convertAsync({ name: 'setup.md', data: Buffer.from('# Dunlin\n') }, { to: 'markdown' });
  if (!result.content.includes('Dunlin')) throw new Error('Docling conversion check failed');
  if (setup && options.tool === 'docling-rs' && options.modelDir) await installDoclingModels(options.modelDir);
  configureDoclingModels(options.modelDir);
  const { ready, tableformer } = checkDependencies();
  if (options.tool === 'docling-rs' && options.modelDir && (!ready || !tableformer)) {
    throw new Error('Docling PDF models are incomplete. Run dunlin setup documents.');
  }
  if (!ready) await pageDependencies();
  return { execution: 'local', layoutModelsReady: ready, tableModelsReady: tableformer, formats: ['docx', 'xlsx', 'pptx', ready ? 'pdf-layout' : 'pdf-text'] };
}

export async function parseDocument(bytes, name, options, signal) {
  if (options.tool === 'docling-serve') {
    const form = new FormData();
    form.append('files', new Blob([bytes]), name);
    form.append('to_formats', 'md');
    form.append('image_export_mode', 'placeholder');
    form.append('do_ocr', 'false');
    try {
      const result = await doclingRequest(options, 'v1/convert/file', form, signal);
      if (result.status !== 'success') throw new Error(`Docling Serve conversion did not complete (${result.status ?? 'unknown'})`);
      return { ...markdown(result.document?.md_content, signal), locations: result.document?.dunlin_locations };
    } catch (error) {
      if (!(error instanceof ServiceUnavailable)) throw error;
      try {
        const convert = await localDocling(options.modelDir);
        return await convert(bytes, name, signal);
      } catch (localError) {
        signal?.throwIfAborted();
        throw new Error(`${error.message}. Local Docling failed: ${localError.message}`, { cause: localError });
      }
    }
  }
  const convert = await localDocling(options.modelDir);
  return convert(bytes, name, signal);
}

export async function localDocling(modelDir, reusePipeline = false) {
  configureDoclingModels(modelDir);
  const docling = await nativeDocling();
  const { ready } = docling.checkDependencies();
  let pipeline;
  return async (bytes, name, signal) => {
    signal?.throwIfAborted();
    const pdf = extname(name).toLowerCase() === '.pdf';
    if (pdf && !ready) {
      const { content, locations } = await pdfText(bytes, signal);
      return { ...markdown(content, signal), tool: 'pdfjs', locations };
    }
    const converter = reusePipeline && pdf
      ? (pipeline ??= new docling.Pipeline({ noOcr: true })) : docling;
    const result = await converter.convertAsync({ name, data: bytes }, { to: 'markdown', imageMode: 'placeholder', noOcr: true });
    if (result.status && result.status !== 'success') throw new Error(`Docling conversion did not complete (${result.status})`);
    return { ...markdown(result.content, signal), tool: 'docling-rs' };
  };
}

export async function pdfText(bytes, signal) {
  return withPdf(bytes, async (document) => {
    const pages = [];
    const locations = [];
    let startLine = 1;
    for (let number = 1; number <= document.numPages; number++) {
      signal?.throwIfAborted();
      const page = await document.getPage(number);
      const text = (await pdfPageText(page)).trim();
      page.cleanup();
      if (!text) continue;
      pages.push(text);
      const endLine = startLine + text.split('\n').length - 1;
      locations.push({ page: number, range: { startLine, endLine } });
      startLine = endLine + 2;
    }
    return { content: pages.join('\n\n'), locations };
  });
}

function markdown(content, signal) {
  signal?.throwIfAborted();
  if (typeof content !== 'string' || !content.trim()) throw new Error('Document has no text. For scans, configure dunlin setup ocr.');
  return { content, format: 'markdown' };
}

async function doclingRequest({ baseUrl, apiKey }, route, body, signal) {
  const response = await fetchService('Docling Serve', baseUrl, route, {
    method: body ? 'POST' : 'GET', body,
    headers: apiKey ? { 'X-Api-Key': apiKey } : undefined,
  }, signal);
  if (!response.ok) {
    await response.body?.cancel();
    throw new Error(`Docling Serve ${route} returned HTTP ${response.status}`);
  }
  if (body) return response.json();
  await response.body?.cancel();
}

