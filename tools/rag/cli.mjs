#!/usr/bin/env node
// The npm entry point of the CLI (spec 28.2 and 28.13 step 6c). The commands run in Rust; this only
// starts them, stops a running server on SIGINT or SIGTERM, and returns the exit code.
import { channel } from 'node:diagnostics_channel';
import { createRequire } from 'node:module';
import { homedir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { KnowledgeNest } from 'knowledge-nest';

/** The `tools` folder of the platform package, which ships the hannoy binary when the package lacks one. */
function nativeTools() {
  const abi = { win32: '-msvc', linux: '-gnu', darwin: '' }[process.platform];
  if (abi === undefined) return undefined;
  try {
    return join(dirname(createRequire(import.meta.url).resolve('knowledge-nest-' + process.platform + '-' + process.arch + abi)), 'tools');
  } catch (error) {
    if (error.code !== 'MODULE_NOT_FOUND') throw error;
    return undefined;
  }
}

const controller = new AbortController();
const stop = () => controller.abort();
// For tracing, as diagnostics_channel subscribers see it: `{ path }` of each index a server opens.
const indexOpens = channel('knowledge-nest:index-open');

process.exitCode = await KnowledgeNest.runCli(process.argv.slice(2), {
  execPath: process.execPath,
  cli: fileURLToPath(import.meta.url),
  package: fileURLToPath(new URL('../..', import.meta.url)),
  nativeTools: nativeTools(),
  home: homedir(),
  serving(active) {
    if (active) {
      process.once('SIGINT', stop);
      process.once('SIGTERM', stop);
    } else {
      process.off('SIGINT', stop);
      process.off('SIGTERM', stop);
    }
  },
  opened: (path) => indexOpens.publish({ path }),
}, { signal: controller.signal });
