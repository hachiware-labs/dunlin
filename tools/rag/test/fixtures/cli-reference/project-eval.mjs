import { readFile } from 'node:fs/promises';

import { stringify } from 'yaml';
import { atomicWrite, revision, withWriteLock } from './authoring/files.mjs';

// Evaluation runs in Rust (spec 28.11). JavaScript creates the tables when a project opens and
// writes the selected evaluation data into the configuration file, since YAML stays in JavaScript
// until stage 6.

class EvaluationError extends Error {
  constructor(exitCode, code, message) {
    super(message);
    this.exitCode = exitCode;
    this.code = code;
  }
}

export function ensureEvaluation(db) {
  db.exec('CREATE TABLE IF NOT EXISTS evaluation_datasets (id TEXT PRIMARY KEY, data TEXT NOT NULL);' +
    'CREATE TABLE IF NOT EXISTS evaluation_reports (id TEXT PRIMARY KEY, tag TEXT UNIQUE, data TEXT NOT NULL);' +
    'CREATE TABLE IF NOT EXISTS evaluation_dataset_tags (name TEXT PRIMARY KEY, dataset_id TEXT NOT NULL);' +
    'CREATE TABLE IF NOT EXISTS evidence_positions (id TEXT PRIMARY KEY, data TEXT NOT NULL);' +
    'CREATE TABLE IF NOT EXISTS evidence_maps (id TEXT PRIMARY KEY, data TEXT NOT NULL);' +
    'CREATE TABLE IF NOT EXISTS evidence_overrides (id TEXT PRIMARY KEY, data TEXT NOT NULL);');
}

export async function selectDataset(project, id) {
  return withWriteLock(project.paths.data, async () => {
    const current = await readFile(project.configPath, 'utf8');
    if (revision(current) !== project.configDigest) throw new EvaluationError(4, 'CONFIG_CHANGED', 'The settings changed; retry the selection.');
    const config = structuredClone(project.config);
    config['eval-data'] = { use: 'eval-data:' + id };
    config.eval ??= { split: 'dev', modules: [
      { use: 'metric.evidence_recall', with: { at: ['context', 10], short_evidence_chars: 40, min_coverage: 0.8, unresolved: 'count_as_missed' } },
      { use: 'metric.latency', with: { percentiles: [50, 95, 99] } },
    ] };
    await atomicWrite(project.configPath, stringify(config));
  });
}

