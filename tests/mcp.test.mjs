import assert from 'node:assert/strict';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import { Client } from '@modelcontextprotocol/client';
import { StdioClientTransport } from '@modelcontextprotocol/client/stdio';
import { Dunlin } from 'dunlin';

test('stdio publishes search and read using the same persisted knowledge base', { timeout: 20_000 }, async () => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-mcp-'));
  const store = join(root, 'index');
  const document = join(root, 'source.md');
  const policy = join(root, 'policy.txt');
  const content = '# OAuth\n日本語の根拠\n[Policy](dunlin://policies/policy.txt)';
  const imported = {
    objectId: 'guide.md', url: 'https://example.invalid/guide#setup', format: 'markdown',
    content: '# Intro\nOverview\n## Setup\nimported evidence',
  };
  const client = new Client({ name: 'dunlin-test', version: '0.1.0' });
  try {
    await writeFile(document, content);
    await writeFile(policy, 'Linked policy evidence');
    const database = await Dunlin.open(store);
    try {
      await database.addSource({ id: 'docs', kind: 'filesystem', path: document });
      await database.addSource({ id: 'policies', kind: 'filesystem', path: policy });
      await database.replaceDocuments('web', [imported]);
      await database.activatePlan('keyword-coverage-v1');
      await database.wiki.build({
        id: 'auth/oauth', title: 'OAuth notes',
        documents: [{ sourceId: 'docs', objectId: 'source.md' }, { sourceId: 'policies', objectId: 'policy.txt' }],
      });
    } finally {
      await database.close();
    }
    await client.connect(new StdioClientTransport({
      command: process.execPath,
      args: [fileURLToPath(new URL('../examples/mcp.mjs', import.meta.url)), store],
    }));
    const { tools } = await client.listTools();
    for (const name of ['knowledge.explore', 'knowledge.read', 'knowledge.search', 'knowledge.wiki_get']) {
      assert.ok(tools.some((tool) => tool.name === name), `Expected MCP tool ${name}`);
    }
    const result = await client.callTool({ name: 'knowledge.search', arguments: { query: '日本語' } });
    assert.equal(result.isError, undefined);
    assert.equal(result.structuredContent.totalHits, 1);
    assert.equal(result.structuredContent.planId, 'keyword-coverage-v1');
    const evidence = result.structuredContent.hits[0].evidence;
    const read = await client.callTool({ name: 'knowledge.read', arguments: { evidence } });
    assert.equal(read.structuredContent.content, content);
    const explored = await client.callTool({
      name: 'knowledge.explore', arguments: { query: '日本語', maxSteps: 5 },
    });
    assert.equal(explored.structuredContent.status, 'complete');
    assert.deepEqual(explored.structuredContent.evidence.map(({ evidence }) => evidence.source.sourceId), ['docs', 'policies']);
    const remote = await client.callTool({ name: 'knowledge.search', arguments: { query: 'imported' } });
    const remoteEvidence = remote.structuredContent.hits[0].evidence;
    assert.equal(remoteEvidence.source.url, imported.url);
    assert.deepEqual(remoteEvidence.range, { startLine: 3, endLine: 4 });
    const remoteRead = await client.callTool({ name: 'knowledge.read', arguments: { evidence: remoteEvidence } });
    assert.equal(remoteRead.structuredContent.content, '## Setup\nimported evidence');
    const wiki = await client.callTool({ name: 'knowledge.wiki_get', arguments: { id: 'auth/oauth' } });
    assert.equal(wiki.structuredContent.page.status, 'current');
    assert.equal(wiki.structuredContent.page.citations.length, 2);
  } finally {
    await client.close();
    await rm(root, { recursive: true, force: true, maxRetries: 5 });
  }
});
