// The JavaScript source stage before it moved to Rust (spec 28.9 step 2a): its stage ID and its
// record in state.sqlite. Kept to check that the Rust version keeps identifiers and state unchanged.
import { createHash } from 'node:crypto';

import { loadCorpus, unsupportedFiles } from '../../project-content.mjs';

const revision = (content) => createHash('sha256').update(content).digest('hex');
const digest = (kind, value) => kind + '-' + revision(JSON.stringify(value));

function saveStage(db, stage, output, reused) {
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
      manifest.groups = output.groups.map(([source, docs]) => [source, docs.map(({ content, ...doc }) => ({ ...doc, text: put(content) }))]);
      db.prepare('INSERT OR IGNORE INTO stage_outputs VALUES (?, ?, ?)').run(stage, output.id, JSON.stringify(manifest));
    }
    db.prepare('INSERT INTO latest_stage VALUES (?, ?) ON CONFLICT(stage) DO UPDATE SET id = excluded.id').run(stage, output.id);
    db.prepare('INSERT INTO source_checks (source_id, checked_at) VALUES (?, ?)').run(output.id, new Date().toISOString());
    db.exec('COMMIT');
  } catch (error) {
    db.exec('ROLLBACK');
    throw error;
  }
}

export async function referenceSourceStage(db, { sources, routes, reject, skipUnsupported, force, modules }, paths, root) {
  const skipped = unsupportedFiles(sources, routes);
  const stats = { files: sources.reduce((sum, source) => sum + source.files.length, 0), skipped };
  if (skipped.length && reject && !skipUnsupported) return { unsupported: stats };
  const warnings = [];
  const groups = await loadCorpus(sources, paths, skipped, warnings, db, force, routes, undefined, root);
  const config_id = digest('cfg', modules);
  const id = digest('src', [config_id, groups, stats.skipped]);
  const reused = Boolean(!force && db.prepare('SELECT 1 FROM stage_outputs WHERE stage = ? AND id = ?').get('source', id));
  const output = { id, config_id, modules, ...stats, groups };
  saveStage(db, 'source', output, reused);
  return { output, reused, warnings };
}
