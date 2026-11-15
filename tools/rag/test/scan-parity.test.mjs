import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, matchesGlob } from 'node:path';
import { test } from 'node:test';

import { Dunlin } from 'dunlin';

import { scanFolder } from '../project-content.mjs';
import { parserRoutes, routeFiles } from '../project-parsers.mjs';
import { scanFolder as referenceScan } from './fixtures/reference-load/project-content.mjs';
import { parserKind, resolveParser } from './fixtures/reference-load/project-parsers.mjs';

const paths = ['a.md', 'A.MD', 'docs/a.md', 'Docs/a.md', 'docs/sub/b.txt', '.hidden/a.md', 'a/.b.md', 'x/y/z.vsdx', 'x', 'x/', 'drawings/pump.vsdx',
  'drawings/a/b/c.vsdx', 'scans/2024/old.pdf', 'scans/old.PDF', 'a1.md', 'a10.md', 'ab.md', 'b.md', 'bb.md', '日本語/資料.md', 'name with space.md',
  'a+b(c).md', '[x].md', 'a/b', 'a/b/', 'node_modules/x.md', 'a.b.c', '.md', 'a/./b', 'a/../b', 'a//b', './a', 'a/.', 'a/./', '.', '..', '', '!x',
  'a/b/./', '#a.md', '!a.md', 'a\\b'];
const patterns = ['**', '*', '**/*', '*.md', '**/*.md', '**/*.MD', 'docs/*.md', 'docs/**', 'docs/**/*.txt', '**/.*.md', 'x/**', 'x/*', '{a,b}.md',
  'a{1..3}.md', 'a{01..10}.md', 'a[bc].md', 'a[!b].md', 'a[[:digit:]].md', '!(b).md', '+(a|b).md', '?(a)b.md', '*(b).md', '@(a|b).md', '@(x|).md',
  '?(a).md', 'drawings/**/*.vsdx', 'scans/**/*.pdf', '*/**/*.pdf', '日本語/*', '*space*', 'a+b(c).md', 'a/b/', '#a.md', '!a.md', '', '?.md', '??.md',
  '**/b', 'a/**/b', '.*', '*.*', 'a.b.*', '{docs,scans}/**', 'Docs/*.md', '**/[Dd]ocs/*.md', 'b', 'a/b', 'a/*/b', 'a', 'a/', '!(b)', 'a/*', '{,a}/b',
  'a/{b,c}/', 'a\\b', 'a\\*'];

test('matches globs exactly as Node.js path.matchesGlob', () => {
  let compared = 0;
  for (const path of paths) {
    for (const pattern of patterns) {
      assert.equal(Dunlin.matchesGlob(path, pattern), matchesGlob(path, pattern), JSON.stringify([path, pattern]));
      compared++;
    }
  }
  assert.ok(compared > 2000);
});

test('scans and routes folders like the JavaScript implementation', async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-scan-parity-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const docs = join(root, 'docs');
  const files = ['a.md', 'B.md', 'c.TXT', 'page.html', 'deck.pptx', 'report.pdf', 'scan.png', 'data.bin', 'drawings/pump.vsdx',
    'drawings/sub/notes.md', '.hidden/x.md', 'node_modules/y.md', 'skip/z.md', '日本語/資料.md', 'あ.md', 'ア.md', 'Ａ.md', '10.md', '2.md',
    'name with space.md', 'Dunlinfile.yaml', 'code/main.rs', 'code/app.TS'];
  for (const file of files) {
    await mkdir(join(docs, file, '..'), { recursive: true });
    await writeFile(join(docs, file), 'x');
  }
  for (const [exclude, projectRoot] of [[[], undefined], [['skip/**', '**/*.bin'], docs], [['**/*.MD'], undefined]]) {
    const options = { path: docs, exclude };
    const expected = await referenceScan([], options, { name: 'source.filesystem', projectRoot });
    const actual = await scanFolder([], options, { name: 'source.filesystem', projectRoot });
    assert.deepEqual(actual, expected, JSON.stringify(exclude));
  }
  await assert.rejects(scanFolder([], { path: join(root, 'missing'), exclude: [] }, { name: 'source.filesystem' }), /ENOENT/u);
  await assert.rejects(scanFolder([], { path: join(docs, 'a.md'), exclude: [] }, { name: 'source.filesystem' }), /Expected a directory for source.filesystem/u);

  const [{ files: scanned }] = await referenceScan([], { path: docs, exclude: [] }, { name: 'source.filesystem' });
  for (const entries of [
    [{ name: 'loader.auto', category: 'auto', args: { ocr: 'none' } }],
    [{ name: 'loader.auto', category: 'auto', args: { ocr: 'loader.ocr' } }],
    [{ name: 'loader.auto', category: 'auto', args: { ocr: 'none' } }, { name: 'loader.ocr', category: 'ocr', args: { match: ['*.png'] } },
      { name: 'drawings', category: 'command', args: { match: ['drawings/**'] } }, { name: 'loader.pdf', category: 'pdf', args: {} }],
    [{ name: 'loader.text', category: 'text', args: { match: ['code/**', '*.md'] } }, { name: 'loader.office', category: 'office', args: {} }],
  ]) {
    const routes = parserRoutes(entries);
    const actual = routeFiles(routes, scanned).map((item) => item && [item.route.name, item.kind]);
    const expected = scanned.map(({ file, objectId }) => {
      const route = resolveParser(routes, file, objectId);
      return route && [route.name, parserKind(route, file)];
    });
    assert.deepEqual(actual, expected, JSON.stringify(entries));
  }
});
