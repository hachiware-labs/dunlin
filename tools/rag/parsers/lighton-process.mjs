import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { createInterface } from 'node:readline';

import { lightonServer, managedOcr } from './lighton-setup.mjs';

let active;

export async function acquireLighton(signal) {
  signal?.throwIfAborted();
  const worker = active ??= startWorker();
  worker.users++;
  const release = async () => {
    if (--worker.users !== 0) return;
    if (active === worker) active = undefined;
    await worker.stop();
  };
  let onAbort;
  try {
    const baseUrl = signal ? await Promise.race([worker.ready, new Promise((_, reject) => {
      onAbort = () => reject(signal.reason);
      signal.addEventListener('abort', onAbort, { once: true });
    })]) : await worker.ready;
    return { baseUrl, release };
  } catch (error) {
    await release();
    throw error;
  } finally {
    if (onAbort) signal.removeEventListener('abort', onAbort);
  }
}

function startWorker() {
  const stopping = new AbortController();
  let child;
  let closed;
  const ready = (async () => {
    const { python, modelDir } = await managedOcr();
    stopping.signal.throwIfAborted();
    child = spawn(python, ['-I', '-u', lightonServer, '--device', 'gpu', '--model-dir', modelDir, '--port', '0', '--parent-stdin'], {
      stdio: ['pipe', 'pipe', 'inherit'], windowsHide: true,
    });
    child.stdin.on('error', () => {});
    closed = new Promise((resolve) => child.once('close', resolve));
    const failed = new Promise((_, reject) => child.once('error', reject));
    const lines = createInterface({ input: child.stdout });
    const timeout = AbortSignal.timeout(180_000);
    try {
      const [line] = await Promise.race([
        once(lines, 'line', { signal: AbortSignal.any([timeout, stopping.signal]) }),
        closed.then((code) => { throw new Error(`LightOnOCR stopped before startup (exit ${code})`); }),
        failed,
      ]);
      const { device, url } = JSON.parse(line);
      if (device !== 'gpu' || !/^http:\/\/127\.0\.0\.1:[1-9]\d{0,4}\/v1$/u.test(url)) {
        throw new Error('LightOnOCR did not report a local GPU endpoint');
      }
      return url;
    } catch (error) {
      if (timeout.aborted) throw new Error('Local LightOnOCR took more than 180 seconds to start', { cause: error });
      throw error;
    } finally {
      lines.close();
      child.stdout.resume();
    }
  })();
  return { users: 0, ready, async stop() {
    stopping.abort();
    await ready.catch(() => {});
    child?.stdin.end();
    await closed;
  } };
}
