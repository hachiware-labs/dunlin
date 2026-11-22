// Spec 28.13 step 6c: the CLI runs in Rust. The same commands on two copies of each project give
// the same output, errors, exit codes and files as the former JavaScript CLI (cli-reference).
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { mkdir, mkdtemp, readFile, rm, utimes, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const clis = {
  reference: fileURLToPath(new URL('./fixtures/cli-reference/cli.mjs', import.meta.url)),
  rust: fileURLToPath(new URL('../cli.mjs', import.meta.url)),
};

function run(cli, args, cwd, env) {
  return new Promise((resolve) => {
    execFile(process.execPath, ['--no-warnings', cli, ...args], { cwd, env: { ...process.env, ...env }, maxBuffer: 64 * 1024 * 1024 },
      (error, stdout, stderr) => resolve({ code: error ? error.code : 0, stdout, stderr }));
  });
}

/**
 * Replaces what differs between two runs: the CLI's path, run IDs, times and durations, and the IDs
 * of what records a time (evaluation data, reports and evidence maps). `labels` numbers each ID in
 * the order it first appears in the scenario.
 */
function normalize(text, root, cli, labels) {
  const variants = (path) => [path, path.replaceAll('\\', '/'), JSON.stringify(path).slice(1, -1), JSON.stringify(path.replaceAll('\\', '/')).slice(1, -1)];
  let out = text;
  for (const [path, label] of [[cli, '<CLI>'], [root, '<ROOT>']]) {
    for (const variant of variants(path)) out = out.replaceAll(variant, label);
  }
  out = out.replace(/\b(run-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}|(?:ed|rep|report|em|map|trial)-[0-9a-f]{16,64})\b/gu, (id) => {
    if (!labels.has(id)) labels.set(id, `<ID ${labels.size}>`);
    return labels.get(id);
  });
  return out
    .replace(/\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?Z/gu, '<TIME>')
    .replace(/"latency_ms":\{[^}]*\}/gu, '"latency_ms":{}')
    .replace(/"(50|95|99)": [\d.e+-]+/gu, '"$1": 0')
    .replace(/"(elapsed_ms|duration_ms|latency_ms|p50|p95|p99|total_ms)":\s*[\d.e+-]+/gu, '"$1":0')
    .replace(/\(\d+(?:\.\d+)? ms\)/gu, '(N ms)')
    .replace(/"acquiredAtMs":\d+/gu, '"acquiredAtMs":0')
    // The index records when it was written, so its size varies by a few bytes.
    .replace(/"(\w+_logical_bytes)":\s*\d+/gu, '"$1":0');
}

const docs = {
  'docs/a.md': '# ポンプ\n\nポンプの保守手順は毎月確認する。\n\n## 点検\n\n圧力計の値を記録する。\n',
  'docs/b.txt': 'Valve inspection notes. Replace the gasket every year.\n',
  'docs/c.html': '<html><body><nav>skip</nav><main><h1>冷却塔</h1><p>冷却塔の清掃は四半期ごと。</p></main></body></html>',
  'docs/d.xyz': 'unsupported',
};

/** The commands of the scenario, run in order on each side. Each is `[args]` or `[args, files]`. */
const steps = [
  ['--help'],
  ['init', 'rag', 'kb', '--json'],
  ['init', 'rag', 'kb'],
  ['init', 'rag', 'r2', '--embedding-model', 'e1', '--llm-model', 'l1', '--ocr'],
  ['init', 'mcp', 'm', '--json'],
  ['init', 'wiki', 'w', '--purpose', '保守手順のまとめ'],
  ['init', 'para', 'p', '--json'],
  ['init', 'wiki', 'w2', '--source', 'x'],
  ['init', 'mcp', 'm2', '--llm-model', 'x'],
  ['init', 'rag', 'r3', '--no-ocr', '--ocr'],
  ['init', 'rag', 'r4', '--ocr-tool', 'lightonocr', '--json'],
  ['config', 'list'],
  ['config', 'set', 'llm', '--model', ' chat-1 ', '--json'],
  ['config', 'set', 'embedding', '--model', 'emb', '--provider', 'openai'],
  ['config', 'set', 'ocr', '--tool', 'lightonocr', '--json'],
  ['config', 'set', 'llm', '--provider', 'nope', '--model', 'x'],
  ['config', 'set', 'foo'],
  ['config', 'list', '--json'],
  ['init', 'rag', 'r5', '--json'],
  ['config', 'unset', 'llm'],
  ['config', 'unset', 'llm', '--json'],
  ['--project', 'kb', 'validate'],
  ['--project', 'kb', 'validate', '--json'],
  ['--project', 'kb', 'modules', 'list', '--json'],
  ['--project', 'kb', 'modules', 'describe', 'retrieval.keyword'],
  ['--project', 'kb', 'modules', 'describe', 'nope', '--json'],
  ['--project', 'r2', 'modules', 'list'],
  ['--project', 'kb', 'add', '--list'],
  ['--project', 'kb', 'add', 'bogus', '--json'],
  ['--project', 'r2', 'remove', 'ocr'],
  ['--project', 'kb', 'defaults', '--json'],
  ['--project', 'r2', 'defaults'],
  ['--project', 'kb', 'build', '--dry-run', '--json'],
  ['--project', 'kb', 'build'],
  ['--project', 'kb', 'build', '--json'],
  ['--project', 'kb', 'build', '--skip-unsupported'],
  ['--project', 'kb', 'source', '--dry-run'],
  ['--project', 'kb', 'status', '--json'],
  ['--project', 'kb', 'search', 'ポンプ'],
  ['--project', 'kb', 'search', 'ポンプ', '--json', '--limit', '2'],
  ['--project', 'kb', 'search', 'gasket', '--budget-chars', '40', '--source-id', 'docs'],
  ['--project', 'kb', 'search', 'x', '--limit', '0'],
  ['--project', 'kb', 'search', 'x', '--source-id', 'bad id', '--json'],
  ['--project', 'kb', 'search'],
  ['--project', 'kb', 'search', 'q', '--bogus'],
  ['--project', 'kb', 'search', 'q', '--limit'],
  ['--project', 'kb', 'tag', 'v1'],
  ['--project', 'kb', 'tag', 'v1', '--json'],
  ['--project', 'kb', 'tag', 'v1', '--move', '--json'],
  ['--project', 'kb', 'tag', 'bad tag'],
  ['--project', 'kb', 'use', 'v1', '--json'],
  ['--project', 'kb', 'use', 'nope'],
  ['--project', 'kb', 'eval', '--json'],
  ['--project', 'kb', 'eval-data', 'add', '--question', 'ポンプの保守はいつ？', '--doc', 'a.md', '--quote', 'ポンプの保守手順は毎月確認する。', '--json'],
  ['--project', 'kb', 'eval-data', 'add', '--question', 'Valve gasket?', '--doc', 'b.txt', '--quote', 'Replace the gasket every year.'],
  ['--project', 'kb', 'eval-data', 'list', '--json'],
  ['--project', 'kb', 'eval-data', 'export', 'nope'],
  ['--project', 'kb', 'evidence-map', 'create', '--json'],
  ['--project', 'kb', 'evidence-map', 'list'],
  ['--project', 'kb', 'eval-data', 'bogus'],
  ['--project', 'kb', 'eval', '--json'],
  ['--project', 'kb', 'eval', '--split', 'nope'],
  ['--project', 'kb', 'report', 'list', '--json'],
  ['--project', 'kb', 'report', 'list'],
  ['--project', 'kb', 'eval', '--tag', 'first', '--json'],
  ['--project', 'kb', 'report', 'show', 'first', '--json'],
  ['--project', 'kb', 'report', 'compare', 'first', 'first'],
  ['--project', 'kb', 'status'],
  ['--project', 'kb', 'evidence-map', 'list', '--json'],
  ['--project', 'kb', 'evidence-map', 'export', 'x'],
  ['--project', 'kb', 'dictionary', 'suggest'],
  ['--project', 'kb', 'dictionary', 'add', 'x.csv'],
  ['--project', 'kb', 'serve', '--print-config', '--json'],
  ['--project', 'kb', 'serve', '--json'],
  ['--project', 'kb', 'serve', '--print-config', '--transport', 'ftp'],
  ['--project', 'kb', 'serve', '--print-config', '--port', '0'],
  ['--project', 'kb', 'ask', 'ポンプ'],
  ['--project', 'kb', 'ask', 'ポンプ', '--json'],
  ['--project', 'kb', 'build', '--from', 'index', '--until', 'source'],
  ['--project', 'kb', 'build', '--tag', 'bad tag'],
  ['--project', 'kb', 'build', '--input', 'v1'],
  ['--project', 'kb', 'ingress', 'docs'],
  ['--project', 'kb', 'validate', '--config', 'kb/bad.yaml'],
  ['--project', 'kb', 'validate', '--config', 'kb/bad.yaml', '--json'],
  ['--project', 'kb', 'validate', '--config', 'kb/invalid.yaml'],
  ['--project', 'kb', 'validate', '--config', 'kb/invalid.yaml', '--json'],
  ['--project', 'kb', 'validate', '--config', 'kb/typeless.yaml'],
  ['--project', 'kb', 'search', 'ポンプ', '--config', 'kb/candidate.yaml', '--json'],
  ['--project', 'kb', 'build', '--config', 'kb/candidate.yaml', '--tag', 'cand', '--skip-unsupported', '--json'],
  ['--project', 'kb', 'eval', '--build', 'cand', '--json'],
  ['--project', 'kb', 'use', 'cand'],
  ['--project', 'kb', 'status'],
  ['--project', 'kb', 'status', '--json'],
  ['--project', 'kb', 'defaults', '--apply', 'search', '--json'],
  ['--project', 'kb', 'defaults', '--apply', 'nope'],
  ['--project', 'm', 'serve', '--print-config', '--json'],
  ['--project', 'm', 'ask', 'q'],
  ['--project', 'm', 'status'],
  ['--project', 'w', 'validate', '--json'],
  ['--project', 'w', 'rules'],
  ['--project', 'w', 'verify', '--json'],
  ['--project', 'w', 'search', 'q'],
  ['--project', 'w', 'serve', '--print-config'],
  ['--project', 'w', 'defaults', '--json'],
  ['--project', 'w', 'ingress', '--dry-run', 'kb/docs', '--json'],
  ['--project', 'w', 'ingress', '--dry-run', 'kb/docs'],
  ['--project', 'w', 'query', 'ポンプ'],
  ['--project', 'w', 'progress', 'missing'],
  ['--project', 'p', 'validate'],
  ['--project', 'p', 'archive', 'x'],
  ['--project', 'nowhere', 'status'],
  ['--project'],
  ['--project', '--json'],
  ['bogus', '--json'],
  ['status'],
];

/**
 * The steps whose output the Rust CLI now writes for people instead of printing JSON or a bare
 * usage line, with the stream that changed. `message` is a `--json` result whose error message
 * changed while its code stayed. Everything else of these steps is still compared.
 */
const rewritten = new Map([
  [['--help'], 'stdout'],
  [['init', 'wiki', 'w2', '--source', 'x'], 'stderr'],
  [['--project', 'kb', 'validate'], 'stdout'],
  [['--project', 'kb', 'modules', 'describe', 'retrieval.keyword'], 'stdout'],
  [['--project', 'r2', 'modules', 'list'], 'stdout'],
  [['--project', 'kb', 'add', '--list'], 'stdout'],
  [['--project', 'r2', 'defaults'], 'stdout'],
  [['--project', 'kb', 'build', '--skip-unsupported'], 'stdout'],
  [['--project', 'kb', 'source', '--dry-run'], 'stdout'],
  [['--project', 'kb', 'search', 'ポンプ'], 'stdout'],
  [['--project', 'kb', 'search', 'gasket', '--budget-chars', '40', '--source-id', 'docs'], 'stdout'],
  [['--project', 'kb', 'search'], 'stderr'],
  [['--project', 'kb', 'search', 'q', '--bogus'], 'stderr'],
  [['--project', 'kb', 'tag', 'v1'], 'stdout'],
  [['--project', 'kb', 'tag', 'bad tag'], 'stderr'],
  [['--project', 'kb', 'eval-data', 'add', '--question', 'Valve gasket?', '--doc', 'b.txt', '--quote', 'Replace the gasket every year.'], 'stdout'],
  [['--project', 'kb', 'evidence-map', 'list'], 'stdout'],
  [['--project', 'kb', 'eval-data', 'bogus'], 'stderr'],
  [['--project', 'kb', 'eval', '--split', 'nope'], 'stderr'],
  [['--project', 'kb', 'report', 'list'], 'stdout'],
  [['--project', 'kb', 'report', 'compare', 'first', 'first'], 'stdout'],
  [['--project', 'kb', 'status'], 'stdout'],
  [['--project', 'kb', 'dictionary', 'add', 'x.csv'], 'stderr'],
  [['--project', 'kb', 'build', '--tag', 'bad tag'], 'stderr'],
  [['--project', 'kb', 'use', 'cand'], 'stdout'],
  [['--project', 'm', 'status'], 'stdout'],
  [['--project', 'w', 'rules'], 'stdout'],
  [['--project', 'w', 'ingress', '--dry-run', 'kb/docs'], 'stdout'],
  [['--project', 'w', 'query', 'ポンプ'], 'stdout'],
  [['--project', 'p', 'validate'], 'stdout'],
  [['bogus', '--json'], 'message'],
].map(([args, stream]) => [JSON.stringify(args), stream]));

const candidates = {
  'kb/bad.yaml': 'init:\n  version: 1\n  type: rag\nsource: [\n',
  'kb/typeless.yaml': 'init:\n  version: 1\n',
};

async function project(root) {
  for (const [path, content] of Object.entries(docs)) {
    await mkdir(join(root, 'kb', path, '..'), { recursive: true });
    await writeFile(join(root, 'kb', path), content);
    // Source stages record modification times; both copies get the same.
    await utimes(join(root, 'kb', path), 1_700_000_000, 1_700_000_000);
  }
}

const files = ['kb/KnowledgeNestfile.yaml', 'r2/KnowledgeNestfile.yaml', 'r4/KnowledgeNestfile.yaml', 'r5/KnowledgeNestfile.yaml', 'm/KnowledgeNestfile.yaml',
  'w/KnowledgeNestfile.yaml', 'w/rules.md', 'p/KnowledgeNestfile.yaml', 'kb/.gitignore', 'global.yaml'];

/**
 * Runs every step with one CLI in `root`. Both CLIs run in the same folder, one after the other,
 * since stage IDs include the file URLs of the documents.
 */
async function scenario(cli, root) {
  await rm(root, { recursive: true, force: true });
  await mkdir(root, { recursive: true });
  const env = { KNOWLEDGE_NEST_GLOBAL_CONFIG: join(root, 'global.yaml'), KNOWLEDGE_NEST_OCR_HOME: join(root, 'ocr-home', 'ocr') };
  const results = [];
  const labels = new Map();
  for (const args of steps) {
    if (args[1] === 'kb' && args[2] === 'build' && args.length === 3) await project(root);
    if (args.includes('kb/bad.yaml') || args.includes('kb/typeless.yaml')) {
      for (const [path, content] of Object.entries(candidates)) await writeFile(join(root, path), content);
    }
    if (args.includes('kb/invalid.yaml') || args.includes('kb/candidate.yaml')) {
      const current = await readFile(join(root, 'kb', 'KnowledgeNestfile.yaml'), 'utf8');
      await writeFile(join(root, 'kb', 'invalid.yaml'), current.replace('use: retrieval.keyword', 'use: retrieval.keywords')
        .replace('top_k: 10', 'top_k: 0'));
      await writeFile(join(root, 'kb', 'candidate.yaml'), current.replace('top_k: 10', 'top_k: 5'));
    }
    const result = await run(cli, args, root, env);
    results.push({ args, code: result.code, stdout: normalize(result.stdout, root, cli, labels), stderr: normalize(result.stderr, root, cli, labels) });
  }
  const saved = {};
  for (const file of files) saved[file] = normalize(await readFile(join(root, file), 'utf8'), root, cli, labels);
  return { results, saved };
}

test('runs the CLI like the former JavaScript CLI', { timeout: 600_000 }, async () => {
  const base = await mkdtemp(join(tmpdir(), 'knowledge-nest-cli-parity-'));
  try {
    const root = join(base, 'work');
    const reference = await scenario(clis.reference, root);
    const rust = await scenario(clis.rust, root);
    for (const [index, expected] of reference.results.entries()) {
      const actual = rust.results[index];
      const key = JSON.stringify(expected.args);
      const stream = rewritten.get(key);
      if (stream === undefined) {
        assert.deepEqual(actual, expected, key);
        continue;
      }
      // The text is the Rust CLI's own; the exit code and the other stream still match.
      assert.equal(actual.code, expected.code, key);
      for (const other of ['stdout', 'stderr'].filter((name) => name !== stream && stream !== 'message')) {
        assert.equal(actual[other], expected[other], key + ' ' + other);
      }
      if (stream === 'message') {
        const strip = (text) => JSON.stringify({ ...JSON.parse(text), errors: JSON.parse(text).errors.map(({ message, ...rest }) => rest) });
        assert.equal(strip(actual.stdout), strip(expected.stdout), key);
        assert.equal(actual.stderr, expected.stderr, key);
      } else {
        assert.notEqual(actual[stream], '', key);
        assert.ok(!actual[stream].trimStart().startsWith('{\n'), key + ' prints text, not JSON');
      }
    }
    const text = (args) => rust.results[steps.findIndex((step) => JSON.stringify(step) === JSON.stringify(args))];
    assert.match(text(['--help']).stdout, /^Knowledge-Nest \d+\.\d+\.\d+ — /u);
    assert.match(text(['--project', 'kb', 'status']).stdout, /^Build +bld-\S+\nSearch +search-\S+\nEvaluation +evaluated: evidence_recall/u);
    assert.match(text(['--project', 'kb', 'build', '--skip-unsupported']).stdout, /^(Built|Reused build) bld-\S+ from 3 of 4 files in \d+ ms\./u);
    assert.match(text(['--project', 'kb', 'search', 'ポンプ']).stdout, /^a\.md — ポンプ\n[\s\S]*\n\n1 result; \d+ of 4000 characters\.\n$/u);
    assert.match(text(['--project', 'kb', 'search', 'q', '--bogus']).stderr, /^Unknown option --bogus for nest search\./u);
    assert.match(text(['--project', 'kb', 'eval-data', 'bogus']).stderr, /^Unknown action for nest eval-data: bogus\. Use add, /u);
    assert.match(text(['--project', 'kb', 'tag', 'v1']).stdout, /^Tagged bld-\S+ as v1\.\n$/u);
    assert.equal(text(['--project', 'w', 'rules']).stdout, '# 運用の決まり\n');
    for (const file of files) assert.equal(rust.saved[file], reference.saved[file], file);
    console.log(`compared ${steps.length} commands and ${files.length} files`);
  } finally {
    await rm(base, { recursive: true, force: true });
  }
});
