import { createHash, randomUUID } from 'node:crypto';
import { createReadStream, createWriteStream } from 'node:fs';
import { mkdir, rename, rm } from 'node:fs/promises';
import { dirname } from 'node:path';
import { Readable, Transform } from 'node:stream';
import { pipeline } from 'node:stream/promises';

export async function download(url, timeout = 180_000) {
  const response = await fetch(url, { signal: AbortSignal.timeout(timeout) }).catch((error) => {
    throw new Error(`Could not download ${url}: ${error.message}`, { cause: error });
  });
  if (!response.ok || !response.body) {
    await response.body?.cancel();
    throw new Error(`Could not download ${url}: HTTP ${response.status}`);
  }
  return response;
}

export async function downloadVerified(url, target, checksum, timeout) {
  const existing = await fileDigest(target).catch((error) => {
    if (error.code !== 'ENOENT') throw error;
  });
  if (existing === checksum) return;
  await mkdir(dirname(target), { recursive: true });
  const temporary = `${target}.${randomUUID()}.tmp`;
  try {
    const response = await download(url, timeout);
    const digest = createHash('sha256');
    const stream = new Transform({ transform(chunk, _encoding, done) { digest.update(chunk); done(null, chunk); } });
    await pipeline(Readable.fromWeb(response.body), stream, createWriteStream(temporary, { flags: 'wx' }));
    if (digest.digest('hex') !== checksum) throw new Error(`SHA-256 mismatch for ${url}`);
    await rename(temporary, target);
  } finally {
    await rm(temporary, { force: true });
  }
}

async function fileDigest(path) {
  const digest = createHash('sha256');
  for await (const chunk of createReadStream(path)) digest.update(chunk);
  return digest.digest('hex');
}
