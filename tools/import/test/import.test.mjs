import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, rm } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

import { KnowledgeNest } from 'knowledge-nest';
import { originalForDigest, readOriginal } from '../originals.mjs';

const runNode = promisify(execFile);
const cli = fileURLToPath(new URL('../cli.mjs', import.meta.url));
const guideKey = 'docs/guide #1.md';
const guideUrl = 's3://test-bucket/docs/guide%20%231.md';

test('imports HTTP and paginated S3 documents and reads their evidence through Knowledge-Nest', { timeout: 40_000 }, async (t) => {
  const requests = [];
  const server = createServer((request, response) => {
    const url = new URL(request.url, 'http://localhost');
    if (url.pathname === '/redirect') {
      response.writeHead(302, { location: '/page' }).end();
      return;
    }
    if (url.pathname === '/page' || url.pathname === '/new-page') {
      const title = url.pathname === '/page' ? 'WebNeedle' : 'NewNeedle';
      response.writeHead(200, { 'content-type': 'text/html; charset=utf-8' });
      response.end(`<nav>navnoise</nav><main><h1>${title}</h1><p>東京 &amp; 大阪</p><p>本文の根拠</p><script>scriptnoise</script></main>`);
      return;
    }

    const key = decodeURIComponent(url.pathname.slice('/test-bucket/'.length));
    const token = url.searchParams.get('continuation-token');
    requests.push({ key, token, auth: request.headers.authorization, ifMatch: request.headers['if-match'] });
    if (url.searchParams.get('list-type') === '2' && url.searchParams.get('prefix') === 'docs/') {
      const keys = token ? ['docs/policy.txt'] : [guideKey, 'docs/image.png'];
      const contents = keys.map((name) => `<Contents><Key>${encodeURIComponent(name)}</Key><ETag>"fixture-etag"</ETag></Contents>`).join('');
      response.writeHead(200, { 'content-type': 'application/xml' });
      response.end(`<ListBucketResult><EncodingType>url</EncodingType><IsTruncated>${!token}</IsTruncated>${token ? '' : '<NextContinuationToken>page2</NextContinuationToken>'}${contents}</ListBucketResult>`);
    } else if (key === guideKey) {
      response.writeHead(200, { 'content-type': 'application/octet-stream' });
      response.end('# Intro\r\nOverview\r\n## Details\r\nS3Needle の根拠\r\n## End\r\nOther');
    } else if (key === 'docs/policy.txt') {
      response.writeHead(200, { 'content-type': 'text/plain' }).end('SecondPageNeedle');
    } else {
      response.writeHead(404, { 'content-type': 'application/xml' }).end('<Error><Code>NoSuchKey</Code><Message>Missing object</Message></Error>');
    }
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  t.after(() => new Promise((resolve) => server.close(resolve)));
  const origin = `http://127.0.0.1:${server.address().port}`;
  const root = await mkdtemp(join(tmpdir(), 'knowledge-nest-import-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const index = join(root, 'index');
  const webUrl = `${origin}/redirect?edition=1`;
  const env = {
    ...process.env,
    AWS_ACCESS_KEY_ID: 'fixture-access',
    AWS_SECRET_ACCESS_KEY: 'fixture-secret',
    AWS_SESSION_TOKEN: 'fixture-token',
    AWS_REGION: 'us-east-1',
    AWS_PROFILE: '',
    AWS_EC2_METADATA_DISABLED: 'true',
  };
  const run = (...locations) => runNode(process.execPath, [
    cli, '--index', index, '--source', 'mixed', '--s3-endpoint', origin, ...locations,
  ], { env, timeout: 15_000 });

  const result = await run(webUrl, 's3://test-bucket/docs/');
  assert.deepEqual(JSON.parse(result.stdout), { files: 3, units: 5 });
  assert.deepEqual(requests.map(({ key, token }) => [key, token]), [
    ['', null], [guideKey, null], ['', 'page2'], ['docs/policy.txt', null],
  ]);
  assert.ok(requests.every(({ auth }) => auth?.startsWith('AWS4-HMAC-SHA256 Credential=fixture-access/')));
  assert.equal(requests[1].ifMatch, '"fixture-etag"');

  async function withIndex(check) {
    const database = await KnowledgeNest.open(index);
    try {
      await check(database);
    } finally {
      await database.close();
    }
  }

  let webEvidence;
  await withIndex(async (database) => {
    webEvidence = (await database.search('WebNeedle')).hits[0].evidence;
    assert.equal(webEvidence.source.url, webUrl);
    assert.deepEqual(webEvidence.range, { startLine: 1, endLine: 3 });
    assert.equal((await database.read(webEvidence)).content, 'WebNeedle\n東京 & 大阪\n本文の根拠');
    const webCorpus = await database.getCorpusDocument(webEvidence.source, webEvidence.source.version);
    assert.equal(webCorpus.content, 'WebNeedle\n東京 & 大阪\n本文の根拠');
    assert.equal(webCorpus.parser.tool, 'cheerio');
    assert.equal(webCorpus.originalSha256, undefined);
    assert.equal((await database.search('navnoise scriptnoise')).totalHits, 0);
    const guide = (await database.search('S3Needle')).hits[0].evidence;
    assert.equal(guide.source.url, guideUrl);
    assert.deepEqual(guide.range, { startLine: 3, endLine: 4 });
    assert.equal((await database.read(guide)).content, '## Details\r\nS3Needle の根拠\r\n');
    const policy = (await database.search('SecondPageNeedle')).hits[0].evidence;
    assert.equal(policy.source.url, 's3://test-bucket/docs/policy.txt');
    assert.equal((await database.read(policy)).content, 'SecondPageNeedle');
  });

  await assert.rejects(run(`${origin}/new-page`, 's3://test-bucket/docs/missing.md'), /NoSuchKey|Missing object/u);
  await withIndex(async (database) => {
    assert.equal((await database.search('NewNeedle')).totalHits, 0);
    assert.equal((await database.read(webEvidence)).evidence.source.url, webUrl);
  });

  const replacement = await run('--keep-original', guideUrl);
  assert.deepEqual(JSON.parse(replacement.stdout), { files: 1, units: 3 });
  await withIndex(async (database) => {
    assert.equal((await database.search('WebNeedle SecondPageNeedle')).totalHits, 0);
    const { source } = (await database.search('S3Needle')).hits[0].evidence;
    assert.equal(source.url, guideUrl);
    const corpus = await database.getCorpusDocument(source, source.version);
    const original = originalForDigest(source, corpus.originalSha256);
    const archiveId = new URL(original.uri).pathname.split('/')[1];
    const bytes = await readOriginal(join(index, '.knowledge-nest-uploads-v1'), archiveId, original.sha256);
    assert.equal(bytes.toString('utf8'), '# Intro\r\nOverview\r\n## Details\r\nS3Needle の根拠\r\n## End\r\nOther');
    assert.equal((await database.getCorpusDocument(webEvidence.source, webEvidence.source.version)).content, 'WebNeedle\n東京 & 大阪\n本文の根拠');
  });
});
