// A project made with a development version, when the tool was called Dunlin, is not read any
// more. The command line says so and what to do, instead of only that no project was found.

import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { test } from 'node:test';

const cli = join(resolve(import.meta.dirname, '..'), 'tools', 'rag', 'cli.mjs');

function said(args, cwd) {
  const ran = spawnSync(process.execPath, [cli, ...args], { cwd, encoding: 'utf8' });
  return { code: ran.status, text: `${ran.stdout}${ran.stderr}` };
}

test('a project of the earlier name is told how to move to the new one', async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'kn-earlier-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const renamed = 'Rename Dunlinfile.yaml to KnowledgeNestfile.yaml';
  const folder = 'Its .dunlin folder is not read any more';

  const empty = said(['build'], root);
  assert.equal(empty.code, 3);
  assert.match(empty.text, /No KnowledgeNestfile\.yaml found\./);
  assert.ok(!empty.text.includes('Dunlin'), empty.text);

  await writeFile(join(root, 'Dunlinfile.yaml'), 'init:\n  version: 1\n  type: mcp\n');
  const config = said(['build'], root);
  assert.equal(config.code, 3);
  assert.ok(config.text.includes(renamed), config.text);
  assert.ok(!config.text.includes(folder), config.text);

  await mkdir(join(root, '.dunlin'));
  const both = said(['search', 'x'], root);
  assert.ok(both.text.includes(renamed) && both.text.includes(folder), both.text);

  // The folder given with --project is the one looked at, not the working directory.
  const elsewhere = said(['status', '--project', root], tmpdir());
  assert.ok(elsewhere.text.includes(renamed), elsewhere.text);
});
