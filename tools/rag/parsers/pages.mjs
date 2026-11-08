export async function pageDependencies() {
  try {
    const canvas = await import('@napi-rs/canvas');
    const pdf = await import('pdfjs-dist/legacy/build/pdf.mjs');
    return { ...canvas, getDocument: pdf.getDocument };
  } catch (error) {
    throw new Error('PDF rendering requires pdfjs-dist and the @napi-rs/canvas binary for this platform. Run npm ci in tools/rag.', { cause: error });
  }
}

export async function withPdf(bytes, read) {
  const { getDocument, createCanvas } = await pageDependencies();
  const task = getDocument({
    data: new Uint8Array(bytes), verbosity: 0, isEvalSupported: false, useSystemFonts: true, maxImageSize: 32_000_000,
  });
  try {
    return await read(await task.promise, createCanvas);
  } finally {
    await task.destroy();
  }
}

export async function pdfCanvas(page, createCanvas) {
  const base = page.getViewport({ scale: 1 });
  const viewport = page.getViewport({ scale: 1540 / Math.max(base.width, base.height) });
  const canvas = createCanvas(Math.min(1540, Math.ceil(viewport.width)), Math.min(1540, Math.ceil(viewport.height)));
  await page.render({ canvas, canvasContext: canvas.getContext('2d'), viewport }).promise;
  return canvas;
}

export async function pdfPageText(page) {
  const { items } = await page.getTextContent();
  return items.map(({ str, hasEOL }) => str ? str + (hasEOL ? '\n' : ' ') : '').join('');
}

export async function imageCanvas(bytes) {
  const { createCanvas, loadImage } = await import('@napi-rs/canvas');
  const image = await loadImage(bytes);
  const scale = Math.min(1, 1540 / Math.max(image.width, image.height));
  const canvas = createCanvas(Math.max(1, Math.round(image.width * scale)), Math.max(1, Math.round(image.height * scale)));
  const context = canvas.getContext('2d');
  context.fillStyle = 'white';
  context.fillRect(0, 0, canvas.width, canvas.height);
  context.drawImage(image, 0, 0, canvas.width, canvas.height);
  return canvas;
}

export function pdfBytes(bytes) {
  return bytes.subarray(0, 5).toString('ascii') === '%PDF-';
}

export async function previewPage(bytes, pageNumber, locations) {
  const canvas = pdfBytes(bytes)
    ? await withPdf(bytes, async (pdf, createCanvas) => pdfCanvas(await pdf.getPage(pageNumber), createCanvas))
    : await imageCanvas(bytes);
  const context = canvas.getContext('2d');
  context.strokeStyle = '#c73922';
  context.fillStyle = '#ffbd3a44';
  context.lineWidth = 4;
  for (const { bbox } of locations) {
    if (!bbox) continue;
    const [left, top, right, bottom] = bbox;
    const x = left * canvas.width / 1000;
    const y = top * canvas.height / 1000;
    const width = (right - left) * canvas.width / 1000;
    const height = (bottom - top) * canvas.height / 1000;
    context.fillRect(x, y, width, height);
    context.strokeRect(x, y, width, height);
  }
  return canvas.toBuffer('image/png');
}
