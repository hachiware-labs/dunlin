// Converts Office documents (.docx, .xlsx, .pptx) to Markdown with docling.rs, the JavaScript-only
// parser that builds still call (spec 28.9 step 2a). PDF and OCR are read in Rust.

async function nativeDocling() {
  try {
    return await import('docling.rs');
  } catch (error) {
    throw new Error(`docling.rs is unavailable on ${process.platform}/${process.arch}. Run npm ci in tools/rag.`, { cause: error });
  }
}

/** Returns `{ content, format: "markdown", parser }` for an Office document. */
export async function convertOffice(bytes, name, signal) {
  signal?.throwIfAborted();
  const docling = await nativeDocling();
  const result = await docling.convertAsync({ name, data: bytes }, { to: 'markdown', imageMode: 'placeholder', noOcr: true });
  if (result.status && result.status !== 'success') throw new Error(`Docling conversion did not complete (${result.status})`);
  signal?.throwIfAborted();
  if (typeof result.content !== 'string' || !result.content.trim()) throw new Error('Document has no text.');
  return { content: result.content, format: 'markdown', parser: { tool: 'docling-rs' } };
}
