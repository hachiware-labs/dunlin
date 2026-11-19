const workTable = 'wiki_work';

export function ensureWikiWork(db) {
  db.exec('CREATE TABLE IF NOT EXISTS wiki_work (id TEXT PRIMARY KEY, data TEXT NOT NULL)');
}

export function workStatus(db, id) {
  if (!db.prepare("SELECT 1 FROM sqlite_master WHERE name = ?").get(workTable)) return null;
  const row = db.prepare('SELECT data FROM wiki_work WHERE id = ?').get(id);
  return row && visibleWork(JSON.parse(row.data));
}

export function latestWork(db) {
  return db.prepare('SELECT data FROM wiki_work ORDER BY rowid DESC LIMIT 12').all().map(({ data }) => visibleWork(JSON.parse(data)));
}

export function queueWork(db, id, command, page, connection) {
  const work = { id, command, page: page || null, status: 'queued', stage: 'queued', queued_at: new Date().toISOString(),
    pid: process.pid, ...(connection && { connection }) };
  db.prepare('INSERT INTO wiki_work VALUES (?, ?)').run(id, JSON.stringify(work));
  return work;
}

function visibleWork(work) {
  if (!['queued', 'running'].includes(work.status) || !Number.isSafeInteger(work.pid) || work.pid <= 0 || work.pid === process.pid) return work;
  try { process.kill(work.pid, 0); }
  catch (error) {
    if (error.code === 'ESRCH') return { ...work, status: 'failed', stage: 'failed',
      error: { code: 'INTERRUPTED', message: 'The process stopped before completing this work.' } };
  }
  return work;
}

export async function trackWork(db, id, command, page, run, onProgress = () => {}) {
  const work = workStatus(db, id) ?? queueWork(db, id, command, page);
  const update = db.prepare('UPDATE wiki_work SET data = ? WHERE id = ?');
  const save = () => update.run(JSON.stringify(work), id);
  const progress = (stage, counts = {}) => {
    work.stage = stage;
    work.progress = counts;
    save();
    onProgress({ run_id: id, stage, ...counts });
  };
  work.status = 'running';
  work.started_at = new Date().toISOString();
  progress('starting');
  try {
    work.result = await run(progress);
    work.status = 'succeeded';
    return work.result;
  } catch (error) {
    work.status = 'failed';
    work.error = { code: error.code ?? 'EXECUTION_FAILED', message: error.message };
    throw error;
  } finally {
    work.stage = work.status;
    work.finished_at = new Date().toISOString();
    save();
    onProgress({ run_id: id, stage: work.stage });
  }
}
