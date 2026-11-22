import assert from 'node:assert/strict';
import { test } from 'node:test';

import { KnowledgeNest } from 'knowledge-nest';

import { describeModules } from './fixtures/cli-reference/project-modules.mjs';
import { describeModules as referenceDescribe, referenceArgumentInfo, referenceModules } from './fixtures/module-registry-reference.mjs';

const inputs = [undefined, null, '', ' ', '  x  ', '　a　', 'x', 'error', 'skip', 'nfkc', 'none', 'lindera', 'trigram', 'bigram',
  'file', 'stdin', 'json', 'fill_budget', 'decline', 'characters', 'loader.ocr', 0, 1, -1, 2, 4, 5, 32, 60, 100, 1000, 1001,
  1.5, 0.55, -1.5, 99999999, NaN, Infinity, -Infinity, true, false, [], ['x'], [''], [1], ['a', ''], ['a', 1], [null],
  {}, { A: 'x' }, { _a1: 'x', B: 'y' }, { '1A': 'x' }, { A: 1 }, { A: 'x', 'b-c': 'y' }, { A: 1, '1A': 'x' }];

function expectedResult(schema, input) {
  const checked = schema.safeParse(input);
  if (!checked.success) return { code: input === undefined ? 'ARG_MISSING' : checked.error.issues[0].code === 'invalid_type' ? 'ARG_TYPE' : 'ARG_RANGE' };
  return { value: checked.data, defaulted: input === undefined && checked.data !== undefined };
}

test('describes modules exactly as the zod registry did', () => {
  assert.equal(JSON.stringify(KnowledgeNest.describeModules()), JSON.stringify(referenceDescribe()));
  assert.equal(JSON.stringify(describeModules()), JSON.stringify(referenceDescribe()));
  assert.equal(JSON.stringify(describeModules('loader.command')), JSON.stringify(referenceDescribe('loader.command')));
  assert.equal(describeModules('missing.module'), undefined);
});

test('reads every argument with the zod codes, values and defaults', () => {
  let compared = 0;
  for (const [name, module] of referenceModules) {
    for (const [key, schema] of Object.entries(module.args)) {
      for (const input of inputs) {
        const [result] = KnowledgeNest.readModuleArgs(name, input === undefined ? {} : { [key]: input }).filter((item) => item.key === key);
        const expected = expectedResult(schema, input);
        const label = name + '.' + key + ' = ' + String(input) + ' ' + JSON.stringify(input);
        if (expected.code) {
          assert.equal(result.code, expected.code, label);
        } else {
          assert.equal(result.code, undefined, label);
          assert.deepEqual(result.value, expected.value, label);
          assert.equal(Boolean(result.defaulted), expected.defaulted, label);
        }
        assert.deepEqual(result.expected, JSON.parse(JSON.stringify(referenceArgumentInfo(schema))), label);
        compared++;
      }
    }
  }
  assert.ok(compared > 2000);
});

test('keeps argument order and copies values', () => {
  const args = { env: { Z_LAST: 'z', A_FIRST: 'a' }, command: ['tool', '{input}'], match: ['**/*.x'] };
  const results = KnowledgeNest.readModuleArgs('loader.command', args);
  assert.deepEqual(results.map(({ key }) => key), Object.keys(referenceModules.get('loader.command').args));
  const env = results.find(({ key }) => key === 'env').value;
  assert.deepEqual(Object.keys(env), ['Z_LAST', 'A_FIRST']);
  results.find(({ key }) => key === 'command').value[0] = 'changed';
  assert.equal(args.command[0], 'tool');
  assert.throws(() => KnowledgeNest.readModuleArgs('missing.module', {}), { code: 'INVALID_ARGUMENT' });
});
