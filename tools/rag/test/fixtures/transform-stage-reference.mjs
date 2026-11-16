// The JavaScript transform stage before it moved to Rust (spec 28.9 step 2b): chunking with its
// cache, chunk.semantic with the embedding model, the stage ID and its record in state.sqlite.
// Kept to check that the Rust version keeps identifiers and state unchanged.
import { createHash } from 'node:crypto';

import { embedderFor } from '../../models.mjs';
import { chunkCorpus } from '../../project-content.mjs';
import { writeTogether } from './reference-load/project-content.mjs';
import { embedBatch } from '../../vectors.mjs';

const revision = (content) => createHash('sha256').update(content).digest('hex');
const digest = (kind, value) => kind + '-' + revision(JSON.stringify(value));

function readStage(db, stage, id) {
  const row = db.prepare('SELECT manifest FROM stage_outputs WHERE stage = ? AND id = ?').get(stage, id);
  if (!row) return null;
  const output = JSON.parse(row.manifest);
  if (output.payloads !== 1) return output;
  const find = db.prepare('SELECT data FROM stage_payloads WHERE id = ?');
  const get = (key) => JSON.parse(find.get(key).data);
  if (stage === 'source') output.groups = output.groups.map(([source, docs]) => [source, docs.map(({ text, ...doc }) => ({ ...doc, content: get(text) }))]);
  if (stage === 'transform') output.chunks = output.chunks.map((group) => group.map(get));
  return output;
}

function saveStage(db, output, reused) {
  db.exec('BEGIN IMMEDIATE');
  try {
    if (!reused) {
      const manifest = { ...output, payloads: 1 };
      const insert = db.prepare('INSERT OR IGNORE INTO stage_payloads VALUES (?, ?)');
      const put = (value) => {
        const json = JSON.stringify(value);
        const key = revision(json);
        insert.run(key, json);
        return key;
      };
      manifest.chunks = output.chunks.map((group) => group.map(put));
      db.prepare('INSERT OR IGNORE INTO stage_outputs VALUES (?, ?, ?)').run('transform', output.id, JSON.stringify(manifest));
    }
    db.prepare('INSERT INTO latest_stage VALUES (?, ?) ON CONFLICT(stage) DO UPDATE SET id = excluded.id').run('transform', output.id);
    if (output.key !== output.id) db.prepare('INSERT OR REPLACE INTO transform_outputs VALUES (?, ?)').run(output.key, output.id);
    db.exec('COMMIT');
  } catch (error) {
    db.exec('ROLLBACK');
    throw error;
  }
}

async function semanticChunks(input, { max_chars, sentence_chars, similarity }, embedder, cache) {
  const passages = chunkCorpus(input, { size: sentence_chars, overlap: 0 });
  const output = [];
  for (const [group, [, documents]] of input.groups.entries()) {
    const chunks = [];
    for (const [i, { content, format }] of documents.entries()) {
      const parts = passages[group][i];
      const key = revision(JSON.stringify(['chunk.semantic-v1', embedder.identity, max_chars, sentence_chars, similarity, input.unicode, format, revision(content)]));
      const saved = cache.read(key);
      if (saved) { chunks.push(saved); continue; }
      const bytes = Buffer.from(content);
      const text = parts.map((part) => part.searchText ?? bytes.subarray(part.byteRange.start, part.byteRange.end).toString());
      const vectors = [];
      const nonempty = text.map((part, index) => ({ part, index })).filter(({ part }) => part.trim());
      for (let offset = 0; offset < nonempty.length; offset += 32) {
        const batch = nonempty.slice(offset, offset + 32);
        const response = await embedBatch(embedder, batch.map(({ part }) => part));
        for (const [j, { index }] of batch.entries()) vectors[index] = response[j];
      }
      const dimension = vectors.find(Boolean)?.length;
      if (vectors.some((vector) => vector.length !== dimension)) throw new Error('Embedding dimensions differ within the document.');
      const merged = [];
      let length = 0;
      for (const [j, part] of parts.entries()) {
        const width = [...bytes.subarray(part.byteRange.start, part.byteRange.end).toString()].length;
        const previous = merged.at(-1);
        const related = vectors[j] && vectors[j - 1]
          && vectors[j].reduce((sum, n, coordinate) => sum + n * vectors[j - 1][coordinate], 0) >= similarity;
        if (previous && length + width <= max_chars && related) {
          previous.range.endLine = part.range.endLine;
          previous.byteRange.end = part.byteRange.end;
          if (input.unicode === 'nfkc') previous.searchText += text[j];
          length += width;
        } else {
          merged.push({ ...part, range: { ...part.range }, byteRange: { ...part.byteRange } });
          length = width;
        }
      }
      cache.write(key, merged);
      chunks.push(merged);
    }
    output.push(chunks);
  }
  return output;
}

/** `modules` is moduleSettings(plan); `embedder` the settings of embeddingSettings. */
export async function referenceTransformStage(db, { sourceId, modules, force, embedder: settings }) {
  const source = readStage(db, 'source', sourceId);
  const semantic = modules.some(({ use }) => use === 'chunk.semantic');
  const embedder = semantic && embedderFor(settings);
  const config_id = digest('cfg', embedder ? [modules, embedder.identity] : modules);
  const id = digest('trn', [source.id, config_id]);
  const latest = semantic && db.prepare('SELECT output_id FROM transform_outputs WHERE key = ?').get(id);
  const saved = !force && readStage(db, 'transform', latest?.output_id ?? id);
  if (saved) {
    saveStage(db, saved, true);
    return { output: saved, reused: true };
  }
  const read = db.prepare('SELECT chunks FROM document_chunks WHERE id = ?');
  const write = db.prepare(force && semantic ? 'INSERT OR REPLACE INTO document_chunks VALUES (?, ?)' : 'INSERT OR IGNORE INTO document_chunks VALUES (?, ?)');
  const cache = {
    hash: revision,
    read: (key) => { const row = !force && read.get(key); return row && JSON.parse(row.chunks); },
    write: (key, value) => write.run(key, JSON.stringify(value)),
    together: (run) => writeTogether(db, run),
  };
  let input = { groups: source.groups };
  let chunks;
  for (const { use, with: args } of modules) {
    if (use === 'text.normalize') input = { ...input, unicode: args.unicode };
    if (use === 'chunk.length') chunks = chunkCorpus(input, args, cache);
    if (use === 'chunk.structure') chunks = chunkCorpus(input, { size: args.max_chars, overlap: 0, structure: true }, cache);
    if (use === 'chunk.semantic') chunks = await semanticChunks(input, args, embedder, cache);
  }
  const outputId = semantic ? digest('trn', [id, chunks]) : id;
  const output = { id: outputId, key: id, config_id, source_id: source.id, modules, chunks };
  saveStage(db, output, false);
  return { output, reused: false };
}
