import { KnowledgeNest } from 'knowledge-nest';

// Trial runs, their limits and the counting of model requests run in Rust (spec 28.11 step 4c).
// JavaScript checks and combines the configured limits.
const fields = ['max_runs', 'max_elapsed_ms', 'max_api_requests', 'max_api_input_bytes'];

export function checkTrialLimits(config, issue) {
  const limits = config.trial_limits;
  if (limits === undefined) return;
  if (!KnowledgeNest.trialLimitsValid(limits)) issue('ARG_TYPE', ['trial_limits'], fields, 'Set nonnegative integer limits for experimental runs.');
}

/** The stricter of the adopted and candidate limits. */
export function trialLimits(active, candidate) {
  return Object.fromEntries(fields.flatMap((key) => {
    const limit = Math.min(active?.[key] ?? Infinity, candidate?.[key] ?? Infinity);
    return Number.isFinite(limit) ? [[key, limit]] : [];
  }));
}

/**
 * Settles with `promise`, or rejects with the signal's reason as soon as it aborts, as an aborted
 * fetch did. A native call that is still running stops by itself shortly after.
 */
export function untilAborted(promise, signal) {
  if (!signal) return promise;
  promise.catch(() => {});
  return new Promise((resolve, reject) => {
    const abort = () => reject(signal.reason);
    if (signal.aborted) return abort();
    signal.addEventListener('abort', abort, { once: true });
    promise.then(resolve, reject).finally(() => signal.removeEventListener('abort', abort));
  });
}
