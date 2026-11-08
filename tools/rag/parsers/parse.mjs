import { extname } from 'node:path';

import { codeLanguage } from './code.mjs';
import { parserOptions } from './options.mjs';
import { pdfBytes } from './pages.mjs';

const documentTypes = {
  'application/pdf': '.pdf',
  'application/vnd.openxmlformats-officedocument.wordprocessingml.document': '.docx',
  'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet': '.xlsx',
  'application/vnd.openxmlformats-officedocument.presentationml.presentation': '.pptx',
};
const officeExtensions = ['.docx', '.xlsx', '.pptx'];
const imageExtensions = ['.png', '.jpg', '.jpeg', '.webp'];

export function parserCategory(settings, name, contentType, bytes) {
  const mime = contentType?.split(';')[0].trim().toLowerCase();
  const extension = (Object.hasOwn(documentTypes, mime) ? documentTypes[mime] : extname(name)).toLowerCase();
  if (extension === '.pdf' || (bytes && pdfBytes(bytes))) return settings?.ocr ? 'ocr' : settings?.documents ? 'documents' : null;
  if (imageExtensions.includes(extension) || ['image/png', 'image/jpeg', 'image/webp'].includes(mime)) return settings?.ocr ? 'ocr' : null;
  if (officeExtensions.includes(extension)) return settings?.documents ? 'documents' : null;
  return settings?.code && codeLanguage(name) ? 'code' : null;
}

export async function parseConfigured(settings, bytes, name, contentType, signal) {
  const category = parserCategory(settings, name, contentType, bytes);
  if (!category) return null;
  const options = parserOptions(category, settings[category]);
  let parsed;
  if (category === 'ocr') {
    const { parseOcr } = await import('./ocr.mjs');
    parsed = await parseOcr(bytes, options, signal);
  } else if (category === 'documents') {
    const { parseDocument } = await import('./documents.mjs');
    const mime = contentType?.split(';')[0].trim().toLowerCase();
    const extension = pdfBytes(bytes) ? '.pdf' : Object.hasOwn(documentTypes, mime) ? documentTypes[mime] : '';
    const filename = extension && !name.toLowerCase().endsWith(extension) ? name + extension : name;
    parsed = await parseDocument(bytes, filename, options, signal);
  } else {
    const { parseCode } = await import('./code.mjs');
    parsed = await parseCode(bytes, name);
  }
  if (Buffer.byteLength(parsed.content) > 1024 * 1024) throw new Error('Extracted text exceeds 1 MiB');
  const { tool, model, lang } = options;
  return { ...parsed, parser: { tool: parsed.tool ?? tool, model, lang } };
}
