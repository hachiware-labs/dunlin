// The skill in skills/knowledge-nest tells an agent which commands to run. These tests keep it honest:
// every command and option it names exists, and the workflow it describes runs from the
// command line, ending in the HTML reports its script writes.

import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { cp, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { test } from 'node:test';

const root = resolve(import.meta.dirname, '..');
const cli = join(root, 'tools', 'rag', 'cli.mjs');
const skill = join(root, 'skills', 'knowledge-nest');

function knowledgeNest(args, cwd = root) {
  const ran = spawnSync(process.execPath, [cli, ...args], { cwd, encoding: 'utf8' });
  return { code: ran.status, out: ran.stdout, err: ran.stderr };
}

async function markdown(folder) {
  const files = [];
  for (const entry of await readdir(folder, { withFileTypes: true })) {
    const path = join(folder, entry.name);
    if (entry.isDirectory()) files.push(...(await markdown(path)));
    else if (entry.name.endsWith('.md')) files.push(path);
  }
  return files;
}

/** The pieces of a Markdown file that are code: the lines of fenced blocks and inline spans. */
function code(text) {
  const spans = [];
  let fenced = false;
  for (const line of text.split(/\r?\n/)) {
    if (line.trimStart().startsWith('```')) {
      fenced = !fenced;
    } else if (fenced) {
      spans.push(line.split('#')[0]);
    } else {
      for (const [, span] of line.matchAll(/`([^`]+)`/g)) spans.push(span);
    }
  }
  return spans;
}

test('the skill names only commands and options that exist', async () => {
  const help = knowledgeNest(['--help']).out;
  const commands = new Set([...help.matchAll(/^ {2}([a-z][a-z-]*) {2,}\S/gm)].map(([, name]) => name));
  assert.ok(commands.has('build') && commands.has('results'), 'the help lists the commands');

  // The parser says so when it does not know an option, also next to --help.
  const known = new Map();
  const has = (command, option) => {
    const key = `${command} ${option}`;
    if (!known.has(key)) {
      const said = knowledgeNest([command, `${option}=x`, '--help']);
      known.set(key, !`${said.out}${said.err}`.includes(`Unknown option ${option}`));
    }
    return known.get(key);
  };
  assert.equal(has('build', '--release'), true);
  assert.equal(has('build', '--no-such-option'), false);
  const everywhere = new Set(['--json', '--project', '--help']);
  const wrong = [];
  let seen = 0;
  for (const file of await markdown(skill)) {
    for (const span of code(await readFile(file, 'utf8'))) {
      for (const [, command, rest] of span.matchAll(/(?<![./\w-])knowledge-nest ([a-z][a-z-]*)((?:(?!knowledge-nest ).)*)/g)) {
        seen += 1;
        if (!commands.has(command)) {
          wrong.push(`${file}: knowledge-nest ${command} is not a command`);
          continue;
        }
        for (const [option] of rest.matchAll(/--[a-z][a-z-]*/g)) {
          if (!everywhere.has(option) && !has(command, option)) {
            wrong.push(`${file}: knowledge-nest ${command} has no ${option}`);
          }
        }
      }
    }
  }
  assert.deepEqual(wrong, []);
  assert.ok(seen > 40, `the skill's commands were found (${seen})`);
});

test('the workflow of the skill runs and its script writes the reports', async (t) => {
  const project = await mkdtemp(join(tmpdir(), 'knowledge-nest-skill-'));
  t.after(() => rm(project, { recursive: true, force: true, maxRetries: 5 }));
  const fixtures = join(root, 'tests', 'fixtures');
  await cp(join(fixtures, 'extract', 'original'), join(project, 'docs'), { recursive: true });
  await cp(join(fixtures, 'extract', 'additional'), join(project, 'docs'), { recursive: true });
  await cp(join(fixtures, 'optimize', 'cases.json'), join(project, 'cases.json'));
  await cp(join(fixtures, 'optimize', 'chunk-and-search.yaml'), join(project, 'tune.yaml'));
  const config = await readFile(join(fixtures, 'optimize', 'KnowledgeNestfile.yaml'), 'utf8');
  await writeFile(
    join(project, 'KnowledgeNestfile.yaml'),
    `${config}\neval:\n  - eval.evidence: { data: default, split: dev }\n`,
  );
  const run = (...args) => knowledgeNest(args, project);
  const data = (...args) => {
    const ran = run(...args, '--json');
    assert.equal(ran.code, 0, ran.err || ran.out);
    return JSON.parse(ran.out).data;
  };

  // A staged configuration file sends the commands both stores have to the versioned store.
  const built = run('build', '--release');
  assert.equal(built.code, 0, built.err);
  assert.match(built.out, /Documents: 6 added, 0 updated, 0 deleted, 0 unchanged\./);
  assert.match(built.out, /Served as release\./);

  // Evaluation data that is not there stops an evaluation; then it is saved and used.
  assert.match(run('eval').out + run('eval').err, /There is no evaluation data default\./);
  assert.equal(run('eval-data', 'import', 'cases.json').code, 0);
  assert.equal(data('eval').recall, 1);

  // An option of the store before this one is refused, not passed over.
  const refused = run('build', '--force');
  assert.notEqual(refused.code, 0);
  assert.match(refused.out + refused.err, /--force does not work with a configuration file of this form yet\./);

  const names = data('list', 'indexes').entries.map((entry) => entry.names);
  assert.deepEqual(names, [['main', 'default']]);
  assert.match(run('search', 'ポンプの点検', '--limit', '1').out, /1\. docs\/manuals\/pump\.md/);

  const tuned = data('optimize', 'tune.yaml');
  assert.equal(tuned.name, 'tune');
  assert.equal(tuned.candidates.length, 13);
  const trial = tuned.candidates.find((candidate) => candidate.name === 'chunking=as-is,search=top1');
  const missed = data('results', trial.tag).cases.filter((item) => item.rank === null);
  assert.ok(missed.length > 0 && missed[0].expected.quote && Array.isArray(missed[0].returned));
  assert.deepEqual(data('results').optimizations, ['tune']);

  // An index that is there is served by its name, and the one before by its version.
  const structure = tuned.candidates.find((candidate) => candidate.name === 'chunking=structure,search=top10');
  const released = data('release', structure.tag);
  assert.equal(released.index, structure.index);
  assert.equal(data('release', String(released.before)).index, released.before);

  // A staged file is checked without building, and status shows what list shows.
  assert.match(run('validate').out, /KnowledgeNestfile\.yaml can run: extract, transform, index, eval\./);
  assert.equal(run('status').out, run('list').out);
  const wrong = run('validate', '--config', 'tune.yaml');
  assert.notEqual(wrong.code, 0);
  // What is served is searched by the name it is served under.
  assert.ok(
    run('search', 'ポンプの点検', '--index', 'release', '--limit', '1').out.startsWith(
      `Index release (served, version ${released.before}):`,
    ),
  );

  // The script of the skill writes each kind of report from what `results` gives.
  const report = (name, file, expected) => {
    const out = join(project, file);
    const ran = spawnSync(
      process.execPath,
      [join(skill, 'scripts', 'report.mjs'), ...(name ? [name] : []), '--out', out, '--project', project],
      { encoding: 'utf8', env: { ...process.env, KNOWLEDGE_NEST: `"${process.execPath}" "${cli}"` } },
    );
    assert.equal(ran.status, 0, ran.stderr);
    assert.equal(ran.stdout.trim(), out);
    return readFile(out, 'utf8').then((html) => {
      for (const text of expected) assert.ok(html.includes(text), `${file} has ${text}`);
    });
  };
  await report('tune', 'optimization.html', [
    '<h1>Optimization tune</h1>',
    'Cases by trial',
    'chunking=structure,search=top10',
    'index.vector is not on the new store yet.',
    'chunk.structure',
  ]);
  await report(trial.tag, 'result.html', ['Expected evidence', 'missed', missed[0].expected.quote]);
  await report(undefined, 'overview.html', ['<h1>Evaluation results</h1>', 'tune/000']);
});
