import { spawn } from 'node:child_process';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, extname, join, matchesGlob } from 'node:path';
import { z } from 'zod';

import { resolveConfigValue } from './config.mjs';
import { isPlainDocument } from './ingest-files.mjs';

export const officeExtensions = new Set(['.docx', '.xlsx', '.pptx']);
export const htmlExtensions = new Set(['.html', '.htm']);
export const imageExtensions = new Set(['.png', '.jpg', '.jpeg', '.webp', '.bmp', '.gif']);
const placeholder = /\{\{|\}\}|\{([^{}]*)\}/gu;
const stdoutLimit = 16 * 1024 * 1024;
const textLimit = 1024 * 1024;
const stderrTail = 2048;

// Built-in parsers claim the formats they can read. loader.command claims files by match alone.
const formats = {
  text: (file) => isPlainDocument(file) || htmlExtensions.has(extname(file).toLowerCase()),
  office: (file) => officeExtensions.has(extname(file).toLowerCase()),
  pdf: (file) => extname(file).toLowerCase() === '.pdf',
  ocr: (file) => extname(file).toLowerCase() === '.pdf' || imageExtensions.has(extname(file).toLowerCase()),
  command: () => true,
};
formats.auto = (file) => formats.text(file) || formats.office(file) || formats.pdf(file);

/**
 * Orders the parser modules of a source plan. loader.auto is the fallback, so it is matched last
 * wherever it is written. The other parsers keep the written order.
 */
export function parserRoutes(entries) {
  const routes = entries.map(({ name, module, category, args }) => ({
    name, module, category, match: args.match, args,
    ocr: category === 'ocr' || (category === 'auto' && args.ocr === 'loader.ocr'),
  }));
  return [...routes.filter(({ category }) => category !== 'auto'), ...routes.filter(({ category }) => category === 'auto')];
}

export function defaultRoutes(ocr = false) {
  return parserRoutes([{ name: 'loader.auto', module: 'loader.auto', category: 'auto', args: { ocr: ocr ? 'loader.ocr' : 'none' } }]);
}

export function resolveParser(routes, file, objectId) {
  for (const route of routes) {
    if (route.match && !route.match.some((pattern) => matchesGlob(objectId, pattern))) continue;
    if (formats[route.category](file) || (route.category === 'auto' && route.ocr && formats.ocr(file))) return route;
  }
  return null;
}

/** Selects how a routed file is decoded. */
export function parserKind(route, file) {
  if (route.category === 'command') return 'command';
  const extension = extname(file).toLowerCase();
  if (route.ocr && formats.ocr(file)) return 'ocr';
  if (extension === '.pdf') return 'pdf';
  if (htmlExtensions.has(extension)) return 'html';
  if (officeExtensions.has(extension)) return 'office';
  return 'text';
}

/** Runs version_command once per build and returns the version text. */
export async function commandVersion(route, root, signal) {
  if (route.args.version !== undefined) return route.args.version;
  if (route.args.version_command === undefined) return null;
  const { stdout } = await execute(route.args.version_command, { cwd: root, env: commandEnv(route.args.env),
    timeout: Math.min(route.args.timeout_ms, 30_000), signal });
  const version = decode(stdout).trim();
  if (!version) throw new Error(route.name + ' version_command printed no version');
  return version;
}

export function commandIdentity(route, version) {
  const { command, input, output, env } = route.args;
  return { module: route.name, command, input, output, env, version };
}

/** Parses one file with loader.command. Each route allows args.concurrency processes at a time. */
export async function parseWithCommand(route, bytes, name, { root, version, signal }) {
  const release = await acquire(route);
  let directory;
  try {
    const extension = extname(name).toLowerCase();
    let input;
    if (route.args.input === 'file') {
      directory = await mkdtemp(join(tmpdir(), 'dunlin-parse-'));
      input = join(directory, 'source' + (/^\.[\p{L}\p{N}_-]{1,16}$/u.test(extension) ? extension : ''));
      await writeFile(input, bytes, { flag: 'wx' });
    }
    const values = { input, name: basename(name), ext: extension };
    const argv = route.args.command.map((part, index) => index === 0 ? part : part.replace(placeholder, (text, key) =>
      text === '{{' ? '{' : text === '}}' ? '}' : values[key]));
    const { stdout } = await execute(argv, { cwd: root, env: commandEnv(route.args.env), timeout: route.args.timeout_ms,
      stdin: route.args.input === 'stdin' ? bytes : undefined, signal });
    const parsed = commandOutput(route.args.output, decode(stdout));
    const tool = basename(route.args.command[0]).replace(/\.(exe|com)$/iu, '');
    return { ...parsed, bytes, parser: { tool, module: route.name, ...(version !== null && { version }) } };
  } finally {
    release();
    if (directory) await rm(directory, { recursive: true, force: true, maxRetries: 3 });
  }
}

const locationSchema = z.strictObject({
  range: z.strictObject({ startLine: z.number().int().min(1), endLine: z.number().int().min(1) })
    .refine(({ startLine, endLine }) => endLine >= startLine, 'endLine must not be less than startLine'),
  page: z.number().int().min(1).optional(),
  symbol: z.string().min(1).optional(),
  bbox: z.tuple([0, 1, 2, 3].map(() => z.number().int().min(0).max(1000))).optional(),
});
const jsonSchema = z.strictObject({
  content: z.string(),
  format: z.enum(['markdown', 'text']),
  locations: z.array(locationSchema).optional(),
});

function commandOutput(output, text) {
  let parsed;
  if (output === 'json') {
    let value;
    try { value = JSON.parse(text); } catch (error) { throw new Error('Parser output is not valid JSON: ' + error.message); }
    const checked = jsonSchema.safeParse(value);
    if (!checked.success) {
      const issue = checked.error.issues[0];
      throw new Error('Parser output does not match the JSON format at ' + (issue.path.join('.') || 'the top level') + ': ' + issue.message);
    }
    parsed = checked.data;
  } else parsed = { content: text, format: output };
  if (!parsed.content.trim()) throw new Error('Parser output has no text');
  if (Buffer.byteLength(parsed.content) > textLimit) throw new Error('Extracted text exceeds 1 MiB');
  const lines = parsed.content.split('\n').length - (parsed.content.endsWith('\n') ? 1 : 0);
  const outside = parsed.locations?.find(({ range }) => range.endLine > lines);
  if (outside) throw new Error(`Parser location ends at line ${outside.range.endLine}, after the last line ${lines}`);
  return parsed;
}

function decode(bytes) {
  try { return new TextDecoder('utf-8', { fatal: true, ignoreBOM: false }).decode(bytes); }
  catch { throw new Error('Parser output is not valid UTF-8'); }
}

function commandEnv(env = {}) {
  const values = { ...process.env };
  for (const [key, value] of Object.entries(env)) values[key] = resolveConfigValue(value, 'loader.command.with.env.' + key, true) ?? '';
  return values;
}

function execute(argv, { cwd, env, timeout, stdin, signal }) {
  return new Promise((done, fail) => {
    signal?.throwIfAborted();
    const child = spawn(argv[0], argv.slice(1), { cwd, env, windowsHide: true, stdio: [stdin ? 'pipe' : 'ignore', 'pipe', 'pipe'] });
    const stdout = [];
    let length = 0;
    let stderr = Buffer.alloc(0);
    let failure;
    const stop = (error) => {
      failure ??= error;
      child.kill();
    };
    const timer = setTimeout(() => stop(new Error(`Parser timed out after ${timeout} ms`)), timeout);
    const abort = () => stop(signal.reason ?? new Error('Parsing was cancelled'));
    signal?.addEventListener('abort', abort, { once: true });
    child.stdout.on('data', (chunk) => {
      length += chunk.length;
      if (length > stdoutLimit) stop(new Error('Parser output exceeds 16 MiB'));
      else stdout.push(chunk);
    });
    child.stderr.on('data', (chunk) => {
      stderr = Buffer.concat([stderr, chunk]);
      if (stderr.length > stderrTail) stderr = stderr.subarray(stderr.length - stderrTail);
    });
    if (stdin) {
      child.stdin.on('error', () => {});
      child.stdin.end(stdin);
    }
    child.on('error', (error) => stop(new Error('Could not start ' + argv[0] + ': ' + error.message)));
    child.on('close', (code, killedBy) => {
      clearTimeout(timer);
      signal?.removeEventListener('abort', abort);
      const detail = stderr.toString('utf8').trim();
      if (failure) fail(failure);
      else if (code !== 0) fail(new Error(`Parser exited with ${code ?? killedBy}` + (detail ? ': ' + detail : '')));
      else done({ stdout: Buffer.concat(stdout, length) });
    });
  });
}

const slots = new WeakMap();
async function acquire(route) {
  let state = slots.get(route);
  if (!state) slots.set(route, state = { active: 0, waiting: [] });
  if (state.active >= route.args.concurrency) await new Promise((wake) => state.waiting.push(wake));
  else state.active++;
  return () => {
    const next = state.waiting.shift();
    if (next) next();
    else state.active--;
  };
}
