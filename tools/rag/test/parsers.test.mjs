import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

import { parse, stringify } from 'yaml';

import { checkCommand, commandVersion, parseWithCommand, parserKind, parserRoutes, resolveParser } from '../project-parsers.mjs';

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
  const route = (path) => resolveParser(routes, '/root/' + path, path)?.name ?? null;
  assert.deepEqual(routes.map(({ name }) => name), ['drawings', 'loader.ocr', 'loader.auto']);
  assert.equal(route('drawings/pump.vsdx'), 'drawings');
  assert.equal(route('drawings/notes.md'), 'drawings');
  assert.equal(route('guide.md'), 'loader.auto');
  assert.equal(route('scans/a/old.pdf'), 'loader.ocr');
  assert.equal(route('scans/photo.png'), null);
  assert.equal(route('manual.pdf'), 'loader.auto');
  assert.equal(route('data.bin'), null);
  assert.equal(parserKind(routes[1], '/root/scans/old.pdf'), 'ocr');
  assert.equal(parserKind(routes[2], '/root/manual.pdf'), 'pdf');
  assert.equal(parserKind(routes[2], '/root/page.html'), 'html');
  assert.equal(parserKind(routes[0], '/root/drawings/notes.md'), 'command');

  const scanning = parserRoutes([{ name: 'loader.auto', category: 'auto', args: { ocr: 'loader.ocr' } }]);
  assert.equal(resolveParser(scanning, '/root/photo.png', 'photo.png').name, 'loader.auto');
  assert.equal(parserKind(scanning[0], '/root/photo.png'), 'ocr');
  const narrowed = parserRoutes([{ name: 'loader.pdf', category: 'pdf', args: { match: ['manuals/**'] } }]);
  assert.equal(resolveParser(narrowed, '/root/manuals/a.docx', 'manuals/a.docx'), null);
});

test('checks loader.command placeholders, executables and versions', async (t) => {
  const check = (values) => checkCommand({ input: 'file', env: {}, ...values });
  const fields = (result) => result.issues.map(({ field }) => field.join('.'));
  const ok = check({ command: [process.execPath, fixture, '{input}', '{{literal}}'], version: '1' });
  assert.deepEqual(ok.issues, []);
  assert.equal(ok.warning, undefined);
  assert.deepEqual(fields(check({ command: [process.execPath, '{input}', '{bogus}'], version: '1' })), ['command.2']);
  assert.deepEqual(fields(check({ command: [process.execPath, fixture], version: '1' })), ['command']);
  assert.deepEqual(fields(check({ command: [process.execPath, '{input}'], input: 'stdin', version: '1' })), ['command']);
  assert.deepEqual(fields(check({ command: [process.execPath, '{input}'], version: '1', version_command: [process.execPath, '--version'] })),
    ['version_command']);
  assert.deepEqual(fields(check({ command: [process.execPath, '{input}'], version_command: [process.execPath, '{input}'] })),
    ['version_command.1']);
  const missing = check({ command: ['dunlin-no-such-parser', '{input}'], version: '1' });
  assert.deepEqual(fields(missing), ['command.0']);
  assert.match(missing.issues[0].hint, /was not found/u);
  assert.equal(check({ command: [process.execPath, '{input}'] }).warning.field[0], 'version');

  if (process.platform === 'win32') {
    const root = await mkdtemp(join(tmpdir(), 'dunlin-batch-'));
    t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
    await writeFile(join(root, 'tool.cmd'), '@echo off\r\n');
    const batch = check({ command: [join(root, 'tool.cmd'), '{input}'], version: '1' });
    assert.match(batch.issues[0].hint, /batch file/u);
  }
});

test('runs external parsers and validates their output', async () => {
  const bytes = Buffer.from('ポンプの点検箇所\n');
  const json = await parseWithCommand(commandRoute({ output: 'json', env: { PARSER_MODE: 'json' } }), bytes, 'pump.vsdx', { version: '1' });
  assert.equal(json.content, '# 図面\nポンプの点検箇所\n');
  assert.equal(json.format, 'markdown');
  assert.deepEqual(json.locations, [{ page: 1, range: { startLine: 1, endLine: 2 } }]);
  assert.deepEqual(json.parser, { tool: 'node', module: 'loader.command', version: '1' });
  assert.ok(json.bytes.equals(bytes));

  const stdin = await parseWithCommand(commandRoute({ command: [process.execPath, fixture], input: 'stdin', output: 'text' }),
    bytes, 'pump.vsdx', { version: null });
  assert.equal(stdin.content, '# 図面\nポンプの点検箇所\n');
  assert.equal(stdin.format, 'text');
  assert.deepEqual(stdin.parser, { tool: 'node', module: 'loader.command' });

  await assert.rejects(parseWithCommand(commandRoute({ env: { PARSER_MODE: 'fail' } }), bytes, 'pump.vsdx', { version: null }),
    /exited with 3: fixture could not read the drawing/u);
  await assert.rejects(parseWithCommand(commandRoute({ output: 'json', env: { PARSER_MODE: 'bad-json' } }), bytes, 'pump.vsdx', { version: null }),
    /does not match the JSON format/u);
  await assert.rejects(parseWithCommand(commandRoute({ output: 'json', env: { PARSER_MODE: 'markdown' } }), bytes, 'pump.vsdx', { version: null }),
    /not valid JSON/u);
  await assert.rejects(parseWithCommand(commandRoute({ timeout_ms: 1000, env: { PARSER_MODE: 'sleep' } }), bytes, 'pump.vsdx', { version: null }),
    /timed out after 1000 ms/u);
  const controller = new AbortController();
  const cancelled = parseWithCommand(commandRoute({ env: { PARSER_MODE: 'sleep' } }), bytes, 'pump.vsdx', { version: null, signal: controller.signal });
  setTimeout(() => controller.abort(new Error('stopped by test')), 200);
  await assert.rejects(cancelled, /stopped by test/u);

  const versioned = commandRoute({ version_command: [process.execPath, fixture], env: { PARSER_MODE: 'version', PARSER_VERSION: '2.5' } });
  assert.equal(await commandVersion(versioned, process.cwd()), 'fixture-parser 2.5');
  assert.equal(await commandVersion(commandRoute({ version: '7' })), '7');
  assert.equal(await commandVersion(commandRoute()), null);
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

  const { DatabaseSync } = await import('node:sqlite');
  const state = new DatabaseSync(join(project, '.dunlin', 'state.sqlite'), { readOnly: true });
  const loaded = state.prepare("SELECT id, document FROM loaded_files WHERE id LIKE '[\"loader.command-v1\"%'").all();
  state.close();
  assert.equal(loaded.length, 1);
  const document = JSON.parse(loaded[0].document);
  assert.deepEqual(document.corpus.parser, { tool: 'node', module: 'loader.command', version: '1' });
  assert.deepEqual(document.corpus.locations, [{ page: 1, range: { startLine: 1, endLine: 2 } }]);

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
