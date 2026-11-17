import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { DatabaseSync } from 'node:sqlite';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

import { Dunlin } from 'dunlin';
import { parse, stringify } from 'yaml';

import { ragTemplate, validateRag } from '../project-modules.mjs';
import { parserRoutes, routeFiles } from '../project-parsers.mjs';

const cli = fileURLToPath(new URL('../cli.mjs', import.meta.url));
const fixture = fileURLToPath(new URL('./fixtures/parser.mjs', import.meta.url));
const runNode = promisify(execFile);

function commandRoute(args = {}) {
  return { name: 'loader.command', category: 'command', args: {
    match: ['**'], command: [process.execPath, fixture, '{input}'], input: 'file', output: 'markdown', env: {},
    timeout_ms: 10_000, concurrency: 2, ...args,
  } };
}

test('routes each file to the first claiming parser and matches loader.auto last', () => {
  const routes = parserRoutes([
    { name: 'loader.auto', category: 'auto', args: { unsupported: 'error', ocr: 'none' } },
    { name: 'drawings', category: 'command', args: { match: ['drawings/**/*.vsdx', 'drawings/**/*.md'] } },
    { name: 'loader.ocr', category: 'ocr', args: { match: ['scans/**/*.pdf'] } },
  ]);
  const routed = (list, path) => routeFiles(list, [{ file: '/root/' + path, objectId: path }])[0];
  const route = (path) => routed(routes, path)?.route.name ?? null;
  const kind = (list, path) => routed(list, path)?.kind;
  assert.deepEqual(routes.map(({ name }) => name), ['drawings', 'loader.ocr', 'loader.auto']);
  assert.equal(route('drawings/pump.vsdx'), 'drawings');
  assert.equal(route('drawings/notes.md'), 'drawings');
  assert.equal(route('guide.md'), 'loader.auto');
  assert.equal(route('scans/a/old.pdf'), 'loader.ocr');
  assert.equal(route('scans/photo.png'), null);
  assert.equal(route('manual.pdf'), 'loader.auto');
  assert.equal(route('data.bin'), null);
  assert.equal(kind(routes, 'scans/old.pdf'), 'ocr');
  assert.equal(kind(routes, 'manual.pdf'), 'pdf');
  assert.equal(kind(routes, 'page.html'), 'html');
  assert.equal(kind(routes, 'drawings/notes.md'), 'command');

  const scanning = parserRoutes([{ name: 'loader.auto', category: 'auto', args: { ocr: 'loader.ocr' } }]);
  assert.equal(routed(scanning, 'photo.png').route.name, 'loader.auto');
  assert.equal(kind(scanning, 'photo.png'), 'ocr');
  const narrowed = parserRoutes([{ name: 'loader.pdf', category: 'pdf', args: { match: ['manuals/**'] } }]);
  assert.equal(routed(narrowed, 'manuals/a.docx'), null);
});

test('checks loader.command placeholders, executables and versions', async (t) => {
  // The checks run in the Rust validator; read back the loader.command diagnostics.
  const check = (values) => {
    const config = ragTemplate('mcp', './docs');
    config.source.modules.push({ use: 'loader.command', with: { match: ['*.x'], input: 'file', env: {}, ...values } });
    const result = validateRag(config, join(tmpdir(), 'Dunlinfile.yaml'), () => null);
    const ours = (list) => list.filter(({ module, code }) => module === 'loader.command' && code !== 'ARG_DEFAULT');
    return { errors: ours(result.errors), warnings: ours(result.warnings), entry: result.plan.source.at(-1) };
  };
  const summary = (result) => result.errors.map(({ arg, expected }) => arg + ': ' + expected);
  const ok = check({ command: [process.execPath, fixture, '{input}', '{{literal}}'], version: '1' });
  assert.deepEqual(ok.errors, []);
  assert.deepEqual(ok.warnings, []);
  assert.equal(ok.entry.args.command[0], process.execPath);
  assert.deepEqual(summary(check({ command: [process.execPath, '{input}', '{bogus}'], version: '1' })), ['command: {input}, {name} or {ext}']);
  assert.deepEqual(summary(check({ command: [process.execPath, fixture], version: '1' })), ['command: {input} with input: file']);
  assert.deepEqual(summary(check({ command: [process.execPath, '{input}'], input: 'stdin', version: '1' })), ['command: No {input} with input: stdin']);
  assert.deepEqual(summary(check({ command: [process.execPath, '{input}'], version: '1', version_command: [process.execPath, '--version'] })),
    ['version_command: Either version or version_command']);
  assert.deepEqual(summary(check({ command: [process.execPath, '{input}'], version_command: [process.execPath, '{input}'] })),
    ['version_command: No placeholders']);
  const missing = check({ command: ['dunlin-no-such-parser', '{input}'], version: '1' });
  assert.deepEqual(summary(missing), ['command: An executable file']);
  assert.match(missing.errors[0].hint, /dunlin-no-such-parser on PATH was not found/u);
  const unversioned = check({ command: [process.execPath, '{input}'] });
  assert.deepEqual(unversioned.warnings.map(({ code, arg }) => code + ' ' + arg), ['ARG_RECOMMENDED version']);

  if (process.platform === 'win32') {
    const root = await mkdtemp(join(tmpdir(), 'dunlin-batch-'));
    t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
    await writeFile(join(root, 'tool.cmd'), '@echo off\r\n');
    const batch = check({ command: [join(root, 'tool.cmd'), '{input}'], version: '1' });
    assert.match(batch.errors[0].hint, /batch file/u);
  }
});

test('runs external parsers and validates their output', async (t) => {
  // loader.command runs in Rust; load one file through Dunlin.loadCorpus and read the document back.
  const root = await mkdtemp(join(tmpdir(), 'dunlin-command-'));
  const state = join(root, 'state.sqlite');
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const bytes = Buffer.from('ポンプの点検箇所\n');
  await writeFile(join(root, 'pump.vsdx'), bytes);
  let run = 0;
  const parse = async (args, options = {}) => {
    const route = commandRoute(args);
    const request = { state, corpus: join(root, 'corpus'), originals: join(root, 'originals'), force: true, ocrVersion: 'v', root,
      files: [{ id: 'docs', file: join(root, 'pump.vsdx'), objectId: 'pump.vsdx', kind: 'command', route: route.name + run++,
        command: { module: route.name, args: route.args } }] };
    const { documents } = await Dunlin.loadCorpus(request, { parse: () => { throw new Error('unexpected host parse'); } }, options);
    return documents[0];
  };
  const json = await parse({ output: 'json', env: { PARSER_MODE: 'json' }, version: '1' });
  assert.equal(json.content, '# 図面\nポンプの点検箇所\n');
  assert.equal(json.format, 'markdown');
  assert.deepEqual(json.corpus.locations, [{ range: { startLine: 1, endLine: 2 }, page: 1 }]);
  assert.deepEqual(json.corpus.parser, { tool: 'node', module: 'loader.command', version: '1' });
  assert.equal(await readFile(join(root, 'originals', json.corpus.originalSha256 + '.bin'), 'utf8'), bytes.toString());

  const stdin = await parse({ command: [process.execPath, fixture], input: 'stdin', output: 'text' });
  assert.equal(stdin.content, '# 図面\nポンプの点検箇所\n');
  assert.equal(stdin.format, 'text');
  assert.deepEqual(stdin.corpus.parser, { tool: 'node', module: 'loader.command' });

  await assert.rejects(parse({ env: { PARSER_MODE: 'fail' } }), /Could not load docs:pump\.vsdx\. Parser exited with 3: fixture could not read the drawing/u);
  await assert.rejects(parse({ output: 'json', env: { PARSER_MODE: 'bad-json' } }), /does not match the JSON format/u);
  await assert.rejects(parse({ output: 'json', env: { PARSER_MODE: 'markdown' } }), /not valid JSON/u);
  await assert.rejects(parse({ timeout_ms: 1000, env: { PARSER_MODE: 'sleep' } }), /timed out after 1000 ms/u);
  const controller = new AbortController();
  const cancelled = parse({ env: { PARSER_MODE: 'sleep' } }, { signal: controller.signal });
  setTimeout(() => controller.abort(), 300);
  await assert.rejects(cancelled, { code: 'ABORTED' });

  const versioned = await parse({ version_command: [process.execPath, fixture], env: { PARSER_MODE: 'version', PARSER_VERSION: '2.5' } });
  assert.equal(versioned.corpus.parser.version, 'fixture-parser 2.5');
  await assert.rejects(parse({ version_command: [process.execPath, fixture], env: { PARSER_MODE: 'fail' } }), /Parser exited with 3/u);
});

test('builds with loader.command, reuses its output and reports parser assignments', { timeout: 60_000 }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-command-parser-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const manuals = join(root, 'manuals');
  const project = join(root, 'kb');
  const counter = join(root, 'calls.txt');
  await mkdir(join(manuals, 'drawings'), { recursive: true });
  await writeFile(join(manuals, 'guide.md'), '# ガイド\n作業手順を確認してください。\n');
  await writeFile(join(manuals, 'drawings', 'pump.vsdx'), 'ポンプ図面の点検箇所はシール部です。\n');
  await writeFile(join(manuals, 'notes.bin'), 'not a supported standard file');
  const run = (...args) => runNode(process.execPath, [cli, '--project', project, ...args],
    { cwd: root, env: { ...process.env, DUNLIN_GLOBAL_CONFIG: join(root, 'global.yaml') }, timeout: 20_000, encoding: 'utf8' });
  const json = async (...args) => JSON.parse((await run(...args, '--json')).stdout);
  const calls = async () => (await readFile(counter, 'utf8').catch(() => '')).split('\n').filter(Boolean).length;
  await runNode(process.execPath, [cli, 'init', 'mcp', './kb', '--source', './manuals'],
    { cwd: root, env: { ...process.env, DUNLIN_GLOBAL_CONFIG: join(root, 'global.yaml') }, encoding: 'utf8' });
  const configPath = join(project, 'Dunlinfile.yaml');
  const config = parse(await readFile(configPath, 'utf8'));
  const command = {
    use: 'loader.command',
    with: { match: ['drawings/**/*.vsdx'], command: [process.execPath, fixture, '{input}'], input: 'file', output: 'json',
      version: '1', env: { PARSER_MODE: 'json', PARSER_COUNTER: counter }, timeout_ms: 20_000, concurrency: 2 },
  };
  config.source.modules.push(command);
  await writeFile(configPath, stringify(config));

  const validated = await json('validate');
  assert.deepEqual(validated.errors, []);
  const preview = (await json('build', '--dry-run')).data;
  const parsers = Object.fromEntries(preview.files.map(({ path, parser }) => [path, parser]));
  assert.deepEqual(parsers, { 'drawings/pump.vsdx': 'loader.command', 'guide.md': 'loader.auto', 'notes.bin': null });
  assert.deepEqual(preview.skipped.map(({ path }) => path), ['notes.bin']);
  assert.equal(preview.commands[0].module, 'loader.command');
  assert.equal(await calls(), 0);

  const build = (await json('build', '--skip-unsupported')).data;
  assert.equal(build.documents, 2);
  assert.equal(await calls(), 1);
  const result = (await json('search', '点検箇所')).data;
  assert.equal(result.results[0].evidence.source.objectId, 'drawings/pump.vsdx');
  assert.match(result.context.text, /シール部/u);

  const state = new DatabaseSync(join(project, '.dunlin', 'state.sqlite'), { readOnly: true });
  const loaded = state.prepare("SELECT source_id, object_id, version FROM extractions WHERE key LIKE '[\"loader.command-v1\"%'").all();
  state.close();
  assert.equal(loaded.length, 1);
  const corpus = new DatabaseSync(join(project, '.dunlin', 'corpus', 'corpus.sqlite'), { readOnly: true });
  const { source_id, object_id, version } = loaded[0];
  const record = corpus.prepare('SELECT record FROM revisions WHERE source_id = ? AND object_id = ? AND version = ?').get(source_id, object_id, version);
  corpus.close();
  const document = JSON.parse(Buffer.from(record.record).toString('utf8'));
  assert.deepEqual(document.parser, { tool: 'node', module: 'loader.command', version: '1' });
  assert.deepEqual(document.locations, [{ range: { startLine: 1, endLine: 2 }, page: 1 }]);

  const repeated = (await json('build', '--skip-unsupported')).data;
  assert.equal(repeated.build_id, build.build_id);
  assert.equal(await calls(), 1);

  command.with.timeout_ms = 30_000;
  await writeFile(configPath, stringify(config));
  await json('build', '--skip-unsupported');
  assert.equal(await calls(), 1, 'speed-only arguments reuse the extracted text');

  command.with.version = '2';
  await writeFile(configPath, stringify(config));
  await json('build', '--skip-unsupported');
  assert.equal(await calls(), 2, 'a new version extracts again');

  command.with.version = '3';
  command.with.env.PARSER_MODE = 'fail';
  await writeFile(configPath, stringify(config));
  await assert.rejects(run('build', '--skip-unsupported', '--json'), (error) => {
    assert.equal(error.code, 1);
    assert.match(error.stdout, /drawings\/pump\.vsdx/u);
    assert.match(error.stdout, /fixture could not read the drawing/u);
    return true;
  });

  command.with.command = [process.execPath, fixture, '{path}'];
  await writeFile(configPath, stringify(config));
  await assert.rejects(run('validate', '--json'), (error) => {
    const { errors } = JSON.parse(error.stdout);
    assert.ok(errors.some(({ code, module, arg }) => code === 'REQUIREMENT_MISSING' && module === 'loader.command' && arg === 'command'));
    return true;
  });
});
