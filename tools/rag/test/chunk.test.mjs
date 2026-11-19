import assert from 'node:assert/strict';
import { test } from 'node:test';

import { chunkCorpus } from './fixtures/cli-reference/project-content.mjs';
import { referenceChunks } from './fixtures/chunk-reference.mjs';

// Pieces that stress grapheme clusters, JavaScript whitespace, line terminators and heading rules.
const pieces = [
  'a', 'b', 'Z', '1', 'ア', 'ｱ', 'Ａ', '漢', '字', '。', '.', '!', '?', ' ', '  ', '\t', '\n', '\n', '\n', '\r\n', '\r',
  '#', '##', '###', '#######', '# ', '## 見出し', '　', ' ', '﻿', '\u0085', ' ', ' ',
  'é', '👍🏽', '👨‍👩‍👧', '🇯🇵', 'क्ष', 'षि', '한', '각', 'ﬁ', '①', '㍻', '‍', '\u{1f600}',
];

function random(seed) {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let value = state;
    value = Math.imul(value ^ (value >>> 15), value | 1);
    value ^= value + Math.imul(value ^ (value >>> 7), value | 61);
    return ((value ^ (value >>> 14)) >>> 0) / 4294967296;
  };
}

function documentFrom(next) {
  const length = Math.floor(next() * 220);
  const content = Array.from({ length }, () => pieces[Math.floor(next() * pieces.length)]).join('');
  const lines = content.split('\n').length;
  const locations = Array.from({ length: Math.floor(next() * 4) }, () => {
    const startLine = 1 + Math.floor(next() * lines);
    return { range: { startLine, endLine: startLine }, ...(next() < 0.7 && { page: 1 + Math.floor(next() * 3) }) };
  });
  return { content, format: next() < 0.6 ? 'markdown' : 'text', corpus: { locations } };
}

test('matches the JavaScript chunker on generated documents', () => {
  const next = random(20261115);
  for (let round = 0; round < 400; round++) {
    const documents = Array.from({ length: 1 + Math.floor(next() * 4) }, () => documentFrom(next));
    const options = { size: 1 + Math.floor(next() * 60), overlap: Math.floor(next() * 20), structure: next() < 0.4 };
    const unicode = next() < 0.7 ? 'nfkc' : 'none';
    const actual = chunkCorpus({ groups: [['docs', documents]], unicode }, options)[0];
    documents.forEach((document, index) => {
      assert.deepEqual(actual[index], referenceChunks(document, { ...options, unicode }),
        JSON.stringify({ round, index, options, unicode, content: document.content }));
    });
  }
});

test('matches the JavaScript chunker on real-looking Markdown and keeps the cache key', () => {
  const content = [
    '# 保全手順', '', 'ポンプの点検は毎月最初の営業日に行う。メカニカルシールは2年ごとに交換する。', '',
    '## 交換部品', '', '- シール部（品番 ＡＢ－１２３）', '- ベアリング', '', '   ### 注意', 'Do not run dry! Check it?',
    '#  ', '####### 七つは見出しではない', '\t# タブ字下げ',
  ].join('\n').repeat(20);
  const document = { content, format: 'markdown', corpus: { locations: [{ page: 2, range: { startLine: 40, endLine: 60 } }] } };
  const saved = new Map();
  const cache = { hash: (text) => text.length, read: (key) => saved.get(key), write: (key, value) => saved.set(key, value) };
  for (const options of [{ size: 800, overlap: 100 }, { size: 120, overlap: 30, structure: true }, { size: 5, overlap: 0 }]) {
    const actual = chunkCorpus({ groups: [['docs', [document]]], unicode: 'nfkc' }, options, cache)[0][0];
    assert.deepEqual(actual, referenceChunks(document, options));
  }
  const keys = [...saved.keys()].map((key) => JSON.parse(key));
  assert.deepEqual(keys[1], ['chunk.structure-v1', 'nfkc', 120, 30, 'markdown', content.length, [40]]);
  assert.equal(chunkCorpus({ groups: [['docs', [document]]], unicode: 'nfkc' }, { size: 5, overlap: 0 }, cache)[0][0],
    saved.get(JSON.stringify(keys[2])));
});
