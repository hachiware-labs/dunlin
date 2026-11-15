import assert from 'node:assert/strict';
import { test } from 'node:test';

import { Dunlin } from 'dunlin';

import { ragTemplate, wikiTemplate, writeTemplate } from '../project-modules.mjs';
import { referenceParseConfig } from './fixtures/config-parse-reference.mjs';

const samples = [
  ...['mcp', 'rag'].flatMap((type) => [{}, { embedding: { provider: 'ollama', model: 'e' } }].map((options) => writeTemplate(ragTemplate(type, './docs', options)))),
  ...['wiki', 'para'].map((type) => writeTemplate(wikiTemplate(type, 'Title', 'Purpose'))),
  `# comment\ninit:\n  version: 1\n  type: rag\n\nsource:\n  modules:\n    - use: source.filesystem\n      with:\n        path: ./docs\n        exclude: []\n    -\n      use: loader.auto\n      with: { unsupported: error, ocr: none }\n    - use: x\n      with:\nempty:\nnullish: ~\nquoted: "a: b"\nblock: |\n  line1\n  line2\nfolded: >-\n  a\n  b\nflow: [1, 2, {a: b}]\nnested:\n  - - a\n    - b\n  -\n    k: v\n`,
  'n: [0o17, 0x1F, 1e3, .5, +1, -0, 1_000, 08, 1.0, .inf, -.Inf, .nan, 0x, 1e, "2", \'3\', 12abc, 1.2.3]\n',
  'b: [true, True, TRUE, tRUE, yes, no, on, off, false, False, FALSE]\nz: [null, Null, NULL, ~, "", nULL]\n',
  '1: a\ntrue: b\n~: c\n1.5: d\n"x y": e\n',
  '---\na: 1\n...\n', '', '# only a comment\n', '---\n', 'a: 1\n---\nb: 2\n', 'a: 1\na: 2\n', 'a: &x 1\nb: *x\n', 'a: &x 1\nb: 2\n',
  'a: [1\n', 'a:\n\tb: 1\n', 'a: b: c\n', '- 1\n- 2\n', 'plain text\n', 'a: !!str 1\nb: !!int "2"\nc: !custom x\n',
  'a:\n  - x\n  -\n  - y\n', 'k: "multi\n  line"\n', 'a: 1 # trailing\nb:   # empty with comment\nc: 3\n',
];

const paths = (value, path = []) => [path, ...(Array.isArray(value) ? value.flatMap((item, i) => paths(item, [...path, i]))
  : value && typeof value === 'object' ? Object.entries(value).flatMap(([key, item]) => paths(item, [...path, key])) : [])];

test('reads Dunlinfile text like the JavaScript yaml package', () => {
  for (const text of samples) {
    const expected = referenceParseConfig(text);
    const actual = Dunlin.parseConfig(text);
    const label = JSON.stringify(text);
    if (expected.syntaxError) {
      assert.equal(actual.syntaxError?.message, expected.syntaxError.message, label);
      continue;
    }
    assert.equal(actual.syntaxError, undefined, label + ' ' + JSON.stringify(actual.syntaxError));
    assert.deepEqual(actual.config, expected.config, label);
    const lines = new Map(actual.lines.map(([path, line]) => [JSON.stringify(path), line]));
    const line = (path) => {
      for (let length = path.length; length > 0; length--) {
        const found = lines.get(JSON.stringify(path.slice(0, length)));
        if (found !== undefined) return found;
      }
      return null;
    };
    for (const path of paths(expected.config).slice(1)) {
      assert.equal(line(path), expected.line(path), label + ' ' + JSON.stringify(path));
      assert.equal(line([...path, 'missing']), expected.line([...path, 'missing']), label + ' ' + JSON.stringify(path) + '+missing');
    }
  }
});
