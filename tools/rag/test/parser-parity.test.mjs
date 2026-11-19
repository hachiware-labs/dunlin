// Spec 28.13 step 6a: HTML pages and Office documents are read in Rust and give the text the
// JavaScript parsers gave: cheerio for HTML and the docling.rs npm package for Office, both as
// test/fixtures/cli-reference keeps them.
import assert from 'node:assert/strict';
import { cp, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import { Dunlin } from 'dunlin';
import { parserHost } from './fixtures/cli-reference/project-content.mjs';

const office = fileURLToPath(new URL('./fixtures/office/', import.meta.url));

const pages = {
  'plain.html': '<html><body><h1>見出し</h1><p>本文です。</p></body></html>',
  'main.html': '<!doctype html><html><head><title>題</title><style>p{}</style><script>var a = 1;</script></head><body><nav>メニュー</nav><header>ヘッダー</header><main><h2>手順 &amp; 注意</h2><p>交換手順を<b>確認</b>する。</p><ul><li>一つ目</li><li>二つ目<ul><li>入れ子</li></ul></li></ul><table><tr><td>部品</td><td>周期</td></tr><tr><td>フィルター</td><td>3か月</td></tr></table><div hidden>隠し</div><p>改行<br>の後</p></main><aside>補足</aside><footer>フッター</footer></body></html>',
  'role.html': '<body><div role="main"><h3>役割</h3><section><p>節の本文</p></section></div><article>記事</article></body>',
  'article.html': '<body><p>外</p><article><h4>記事の見出し</h4><blockquote>引用</blockquote><pre>  整形済み\n  テキスト</pre></article></body>',
  'fragment.html': '<p>閉じていない段落<p>次の段落<div>ブロック<span>インライン</span></div>   \n\n   空白だけの行\n 　全角空白　',
  'entities.html': '<p>&lt;tag&gt; &quot;q&quot; &#x41; &copy; &nbsp;x</p><noscript>無効</noscript><template><p>テンプレート</p></template>',
  'bom.html': '﻿<p>BOM 付き</p>',
  'empty.html': '<html><head><title>題だけ</title></head><body></body></html>',
  'crlf.html': '<p>一行目\r\n二行目</p>\r\n<p>三行目</p>',
  'headings.html': '<h5>五</h5><h6>六</h6><p>本文 <h1>段落の中の見出し</h1></p>',
};

test('reads HTML and Office documents like the former JavaScript parsers', { timeout: 120_000 }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-parser-parity-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const files = join(root, 'files');
  await cp(office, files, { recursive: true });
  for (const [name, content] of Object.entries(pages)) await writeFile(join(files, name), content);
  await writeFile(join(files, 'invalid.html'), Buffer.from([0x3c, 0x70, 0x3e, 0xff, 0xfe]));
  await writeFile(join(files, 'broken.docx'), 'not a zip');
  const names = [...Object.keys(pages), 'invalid.html', 'manual.docx', 'parts.xlsx', 'training.pptx', 'broken.docx'];
  const host = parserHost();
  for (const name of names) {
    const kind = name.endsWith('.html') ? 'html' : 'office';
    const bytes = await readFile(join(files, name));
    let expected;
    try {
      const parsed = await host.parse({ kind, name }, bytes);
      expected = { content: parsed.content, format: parsed.format, parser: parsed.parser };
    } catch (error) {
      expected = { error: error.message };
    }
    let actual;
    try {
      const { documents: [document] } = await Dunlin.loadCorpus({
        state: join(root, 'state.sqlite'), corpus: join(root, 'corpus'), originals: join(root, 'originals'), force: true,
        ocrVersion: 'none', root, files: [{ id: 'docs', file: join(files, name), objectId: name, kind, route: 'loader.auto' }],
      });
      actual = { content: document.content, format: document.format, parser: document.corpus.parser };
    } catch (error) {
      // A file that cannot be read stops the load with the parser's message.
      actual = { error: error.message.replace('Could not load docs:' + name + '. ', '') };
    }
    assert.deepEqual(actual, expected, name);
  }
});
