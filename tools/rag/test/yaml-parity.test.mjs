// Spec 28.13 step 6b: configuration files are written in Rust as the yaml package's stringify
// wrote them: the same quoting, block scalars, line folding at 80 columns and indentation.
import assert from 'node:assert/strict';
import { test } from 'node:test';

import { KnowledgeNest } from 'knowledge-nest';
import { stringify } from 'yaml';

const fixed = [
  {}, [], { a: {} }, { a: [] }, { a: null, b: true, c: false, d: 0, f: 0.8, g: 1e21, h: 123456789012, i: -3.5 },
  { s: ['', ' ', 'a ', ' a', 'a:', 'a: b', 'a:b', '#a', 'a #b', 'a#b', '-', '- a', '-a', '?', '? a', ':', 'a\tb', '%a', '@a', '`a', "'a", '"a', '!a', '&a', '*a', '|a', '>a', ',a', '[a', ']a', '{a', '}a'] },
  { s: ['true', 'True', 'TRUE', 'false', 'null', 'Null', '~', '0', '-1', '+1', '0x1F', '0o17', '1.5', '.5', '5.', '1e3', '1E-3', '.inf', '-.Inf', '.nan', '0b1', '1_000', 'yes', 'no', 'on', 'off', '0777', '+.5e2', '1.0.0'] },
  { s: ["it's", 'say "hi"', `both ' and "`, 'back\\slash', 'tab\there', 'ctrl\u0001char', 'del\u007f', 'nbsp x', 'line sep'] },
  { multi: ['a\nb', 'a\n', 'a\n\n', '\na', ' a\nb', 'a\n b', 'a\nb\n\nc', 'a \nb', 'a\n\t', 'x\n---\ny', '--- x', '%x\ny', 'trailing\n  '] },
  { long: 'The quick brown fox jumps over the lazy dog and keeps running across the wide open field until it reaches the river bank.' },
  { nested: { deeper: { deepest: 'The quick brown fox jumps over the lazy dog and keeps running across the wide open field until the end.' } } },
  { list: ['The quick brown fox jumps over the lazy dog and keeps running across the wide open field until the end of it.'] },
  { quotedLong: 'The quick "brown" fox: jumps over the lazy dog and keeps running across the wide open field until it reaches #the river.' },
  { japanese: 'ポンプの保守手順をまとめる。'.repeat(10), mixed: 'ポンプ pump の保守 maintenance を まとめる '.repeat(5) },
  { block: 'First line of a long paragraph that keeps going well past the eighty column limit so it must be folded by the emitter.\nSecond line.\n' },
  { 'key with spaces': 1, 'key: colon': 2, '#hash': 3, 'eval-data': { use: 'eval-data:ed-abc' }, '123': 4, 'true': 5, '': 6 },
  { init: { version: 1, type: 'rag' }, source: { modules: [{ use: 'source.filesystem', with: { path: './docs', include: ['**/*.md'], exclude: [] } }] },
    models: { answerer: { provider: 'ollama', kind: 'chat', base_url: 'http://localhost:11434', model: null } },
    serve: { transport: 'http', host: '127.0.0.1', port: 4318, endpoints: ['responses', 'search', 'mcp'],
      access: { allowed_hosts: ['localhost', '127.0.0.1'], allowed_origins: [], auth: { type: 'none', token_env: null } } } },
  [[1, 2], [], [{ a: [] }], 'x'],
];

/** A deterministic generator of awkward values. */
function* generated(count) {
  let seed = 0x5eed;
  const next = (n) => {
    seed = (seed * 1103515245 + 12345) & 0x7fffffff;
    return seed % n;
  };
  const pieces = ['a', 'word', ' ', '  ', ':', ': ', '#', ' #', '-', '?', '"', "'", '\\', '\n', '\n\n', '\t', '日本語', '😀', '%', '---', '...',
    '0', '1.5', 'true', 'null', '{', '}', '[', ']', ',', '&', '*', '!', '|', '>', '@', '`', 'longwordwithoutspaces', 'x'.repeat(30)];
  const text = () => Array.from({ length: next(40) }, () => pieces[next(pieces.length)]).join('');
  const value = (depth) => {
    const kind = next(depth > 2 ? 4 : 7);
    if (kind === 0) return text();
    if (kind === 1) return next(3) === 0 ? null : next(2) === 0;
    if (kind === 2) return [next(1000), -(next(100) + 1) / 8, next(10) * 1e20][next(3)];
    if (kind === 3) return text();
    if (kind === 4) return Array.from({ length: next(4) }, () => value(depth + 1));
    return Object.fromEntries(Array.from({ length: next(4) }, (_, i) => [next(5) === 0 ? text() : 'k' + i, value(depth + 1)]));
  };
  for (let i = 0; i < count; i++) yield { root: value(0), text: text() };
}

test('writes YAML like the yaml package', () => {
  for (const value of fixed) assert.equal(KnowledgeNest.stringifyYaml(value), stringify(value), JSON.stringify(value));
  for (const value of generated(5000)) assert.equal(KnowledgeNest.stringifyYaml(value), stringify(value), JSON.stringify(value));
});
