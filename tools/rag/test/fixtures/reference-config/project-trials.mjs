// The JavaScript configuration validation that the Rust validator replaced, copied for tests.
import { AsyncLocalStorage } from 'node:async_hooks';
import { performance } from 'node:perf_hooks';
import { z } from 'zod';

const usage = new AsyncLocalStorage();
const fields = {
  max_runs: 'runs',
  max_elapsed_ms: 'elapsed_ms',
  max_api_requests: 'api_requests',
  max_api_input_bytes: 'api_input_bytes',
};
const settings = z.strictObject(Object.fromEntries(Object.keys(fields).map((key) => [key, z.number().int().min(0).optional()])));

export function checkTrialLimits(config, issue) {
  const limits = config.trial_limits;
  if (limits === undefined) return;
  const checked = settings.safeParse(limits);
  if (!checked.success) issue('ARG_TYPE', ['trial_limits'], Object.keys(fields), 'Set nonnegative integer limits for experimental runs.');
}

export function trialLimits(active, candidate) {
  return Object.fromEntries(Object.keys(fields).flatMap((key) => {
    const limit = Math.min(active?.[key] ?? Infinity, candidate?.[key] ?? Infinity);
    return Number.isFinite(limit) ? [[key, limit]] : [];
  }));
}

function ensure(db) {
  db.exec(`CREATE TABLE IF NOT EXISTS trial_runs (
    id TEXT PRIMARY KEY, operation TEXT NOT NULL, started INTEGER NOT NULL,
    elapsed_ms INTEGER, api_requests INTEGER NOT NULL DEFAULT 0, api_input_bytes INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'running'
  )`);
}

export function trialUsage(db) {
  if (!db.prepare("SELECT 1 FROM sqlite_master WHERE name = 'trial_runs'").get()) {
    return { runs: 0, elapsed_ms: 0, api_requests: 0, api_input_bytes: 0 };
  }
  return db.prepare(`SELECT count(*) AS runs, coalesce(sum(elapsed_ms), 0) AS elapsed_ms,
    coalesce(sum(api_requests), 0) AS api_requests, coalesce(sum(api_input_bytes), 0) AS api_input_bytes FROM trial_runs`).get();
}

function exhausted(field, limits, current) {
  return Object.assign(new Error('Trial limit reached: ' + field + '.'), {
    code: 'TRIAL_LIMIT_REACHED', exitCode: 4, data: { limit: field, maximum: limits[field], used: current },
  });
}

export async function runTrial(db, id, operation, limits, run) {
  ensure(db);
  let total;
  db.exec('BEGIN IMMEDIATE');
  try {
    // The caller holds the execution lock. A prior unfinished row can only be from an interrupted run.
    db.prepare("UPDATE trial_runs SET status = 'interrupted', elapsed_ms = coalesce(elapsed_ms, 0) WHERE status = 'running'").run();
    total = trialUsage(db);
    for (const key of ['max_runs', 'max_elapsed_ms']) {
      if (total[fields[key]] >= (limits[key] ?? Infinity)) throw exhausted(key, limits, total[fields[key]]);
    }
    db.prepare('INSERT INTO trial_runs (id, operation, started) VALUES (?, ?, ?)').run(id, operation, Date.now());
    db.exec('COMMIT');
  } catch (error) { db.exec('ROLLBACK'); throw error; }

  const started = performance.now();
  const available = (limits.max_elapsed_ms ?? Infinity) - total.elapsed_ms;
  const controller = new AbortController();
  const check = () => {
    const elapsed = Math.floor(performance.now() - started);
    if (elapsed >= available && !controller.signal.aborted) {
      controller.abort(exhausted('max_elapsed_ms', limits, total.elapsed_ms + elapsed));
    }
    controller.signal.throwIfAborted();
  };
  const timer = available <= 2147483647 ? setTimeout(() => {
    controller.abort(exhausted('max_elapsed_ms', limits, total.elapsed_ms + Math.ceil(performance.now() - started)));
  }, available) : null;
  const updateElapsed = db.prepare('UPDATE trial_runs SET elapsed_ms = ? WHERE id = ?');
  const clock = setInterval(() => {
    try { updateElapsed.run(Math.ceil(performance.now() - started), id); }
    catch (error) { controller.abort(error); }
  }, 1000);
  let status = 'failed';
  try {
    const result = await usage.run({ check, signal: controller.signal, charge(bytes) {
      check();
      // A project can have only one active trial. Each request is recorded before sending it.
      for (const [key, needed] of Object.entries({ max_api_requests: 1, max_api_input_bytes: bytes })) {
        if (total[fields[key]] + needed > (limits[key] ?? Infinity)) throw exhausted(key, limits, total[fields[key]]);
      }
      db.prepare('UPDATE trial_runs SET api_requests = api_requests + 1, api_input_bytes = api_input_bytes + ? WHERE id = ?').run(bytes, id);
      total.api_requests++;
      total.api_input_bytes += bytes;
    } }, async () => { check(); return run(controller.signal); });
    status = 'succeeded';
    return result;
  } catch (error) {
    const reason = controller.signal.aborted ? controller.signal.reason : error;
    if (reason.code === 'TRIAL_LIMIT_REACHED') status = 'limited';
    throw reason;
  } finally {
    if (timer) clearTimeout(timer);
    clearInterval(clock);
    db.prepare('UPDATE trial_runs SET elapsed_ms = ?, status = ? WHERE id = ?')
      .run(Math.ceil(performance.now() - started), status, id);
  }
}

export function trialCheckpoint() {
  usage.getStore()?.check();
}

export function trialSignal(signal) {
  const current = usage.getStore()?.signal;
  return current && signal && current !== signal ? AbortSignal.any([current, signal]) : current ?? signal;
}

export function chargeTrialApi(body, signal) {
  const current = usage.getStore();
  current?.charge(typeof body === 'string' ? Buffer.byteLength(body) : body?.byteLength ?? 0);
  return trialSignal(signal);
}
