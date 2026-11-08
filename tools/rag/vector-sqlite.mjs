import { randomUUID } from 'node:crypto';
import { access } from 'node:fs/promises';

export function sqliteStore(path) {
  return {
    async save({ revision, dimensions, entries }, beforePublish) {
      const database = await open(path);
      try {
        database.exec(`
          CREATE TABLE IF NOT EXISTS snapshot (id INTEGER PRIMARY KEY, metadata TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS vectors (id INTEGER PRIMARY KEY, source_id TEXT NOT NULL, evidence TEXT NOT NULL, vector BLOB NOT NULL);
          CREATE INDEX IF NOT EXISTS vectors_source ON vectors(source_id);
          BEGIN IMMEDIATE;
          DELETE FROM vectors;
        `);
        const insert = database.prepare('INSERT INTO vectors (source_id, evidence, vector) VALUES (?, ?, ?)');
        for (const { evidence, vector } of entries) {
          insert.run(evidence.source.sourceId, JSON.stringify(evidence), vectorBytes(vector));
        }
        const metadata = { version: 1, revision, dimensions, count: entries.length, generation: randomUUID() };
        database.prepare('INSERT OR REPLACE INTO snapshot (id, metadata) VALUES (1, ?)').run(JSON.stringify(metadata));
        await beforePublish();
        database.exec('COMMIT');
      } finally {
        database.close();
      }
    },
    async load(sourceIds) {
      await access(path);
      const database = await open(path, true);
      try {
        database.exec('BEGIN');
        const snapshot = readSnapshot(database);
        if (snapshot && sourceIds) {
          snapshot.count = database.prepare(`SELECT count(*) AS count FROM vectors ${scope(sourceIds)}`).get(...sourceIds).count;
        }
        return snapshot;
      } finally {
        database.close();
      }
    },
    async search(snapshot, vector, { sourceIds, limit }) {
      const database = await open(path, true);
      try {
        database.exec('BEGIN');
        if (readSnapshot(database)?.generation !== snapshot.generation) {
          throw new Error('SQLite vector index was rebuilt during search. Retry the question.');
        }
        const query = database.prepare(`
          SELECT evidence, min(vec_distance_cosine(vector, ?)) AS distance
          FROM vectors ${scope(sourceIds)}
          GROUP BY evidence
          ORDER BY distance, min(id)
          LIMIT ?
        `);
        return query.all(vectorBytes(vector), ...(sourceIds ?? []), limit).map(({ evidence, distance }) => ({
          evidence: JSON.parse(evidence), score: 1 - distance,
        }));
      } finally {
        database.close();
      }
    },
  };
}

async function open(path, readOnly = false) {
  const [{ DatabaseSync }, sqliteVec] = await Promise.all([import('node:sqlite'), import('sqlite-vec')]);
  const database = new DatabaseSync(path, { readOnly, allowExtension: true });
  try {
    sqliteVec.load(database);
    database.enableLoadExtension(false);
    database.exec('PRAGMA busy_timeout = 5000');
    return database;
  } catch (error) {
    database.close();
    throw error;
  }
}

function readSnapshot(database) {
  const row = database.prepare('SELECT metadata FROM snapshot WHERE id = 1').get();
  return row && JSON.parse(row.metadata);
}

function scope(sourceIds) {
  return sourceIds ? `WHERE source_id IN (${sourceIds.map(() => '?').join(',')})` : '';
}

function vectorBytes(vector) {
  return new Uint8Array(Float32Array.from(vector).buffer);
}
