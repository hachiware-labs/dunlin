import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { KnowledgeNest } from 'knowledge-nest';

import { chunkCorpus } from './fixtures/cli-reference/project-content.mjs';
import { sourceRange } from './fixtures/source-range-reference.mjs';

const content = [
  '# 保全手順', 'ポンプの点検は毎月行う。👍🏽 確認済み。', '', '## シール', 'メカニカルシールは2年ごとに交換する。',
  'Ｅ́ é ①', ' 行区切り\r', '最後の行は改行なし',
].join('\n');

test('describes evidence like the JavaScript offsets it replaced', async (t) => {
  const store = await mkdtemp(join(tmpdir(), 'knowledge-nest-describe-'));
  t.after(() => rm(store, { recursive: true, force: true, maxRetries: 5 }));
  const locations = [{ page: 1, range: { startLine: 1, endLine: 3 } }, { page: 2, range: { startLine: 4, endLine: 8 } },
    { symbol: 'Seal', range: { startLine: 4, endLine: 5 } }];
  const document = { objectId: 'manual.md', url: 'file:///manual.md', format: 'markdown', content, corpus: { locations } };
  const chunks = chunkCorpus({ groups: [['docs', [document]]], unicode: 'nfkc' }, { size: 12, overlap: 3 })[0][0];
  const database = await KnowledgeNest.open(store);
  try {
    await database.replaceDocuments('docs', [{ ...document, chunks }]);
    const { evidence: whole } = await database.getDocument({ sourceId: 'docs', objectId: 'manual.md' });
    const corpus = await database.getCorpusDocument(whole.source, whole.source.version);
    const hits = (await database.search('シール 交換', { mode: 'keyword', limit: 20 })).hits.map(({ evidence }) => evidence);
    assert.ok(hits.some(({ byteRange }) => byteRange), 'search returns byte ranges for chunks');
    const lines = content.split('\n').length;
    const ranges = [];
    for (let startLine = 1; startLine <= lines; startLine++) {
      for (let endLine = startLine; endLine <= lines; endLine++) ranges.push({ ...whole, range: { startLine, endLine }, byteRange: undefined });
    }
    const evidence = [...hits, ...ranges.map(({ byteRange, ...item }) => item)];
    const described = await database.describeEvidence(evidence);
    evidence.forEach((item, index) => {
      const expected = sourceRange(corpus, item);
      const { text, start, end, locations: found } = described[index];
      assert.deepEqual({ range: [start, end], text }, expected, JSON.stringify(item.range));
      assert.deepEqual(found, corpus.locations.filter(({ range }) =>
        range.startLine <= item.range.endLine && range.endLine >= item.range.startLine));
    });
    const missing = { ...whole, source: { ...whole.source, version: 'f'.repeat(64) } };
    assert.deepEqual(await database.describeEvidence([missing]), [null]);
    await assert.rejects(database.describeEvidence([{ ...whole, byteRange: { start: 3, end: 5 } }]), { code: 'INVALID_ARGUMENT' });
  } finally {
    await database.close();
  }
});
