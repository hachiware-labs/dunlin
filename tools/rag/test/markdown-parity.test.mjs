// Spec 28.12 step 5a: Wiki and PARA pages render Markdown in Rust as markdown-it 15 rendered it
// with { html: false }: the same markup, links checked and encoded the same way, raw HTML as text.
import assert from 'node:assert/strict';
import { test } from 'node:test';

import { KnowledgeNest } from 'knowledge-nest';
import MarkdownIt from 'markdown-it';

const markdownIt = new MarkdownIt({ html: false });

const samples = [
  '', 'text', '# 見出し\n\n本文です。', '## a\n### b\n#### c\n##### d\n###### e', 'Setext\n===\n\nTwo\n---',
  '*em* **strong** ***both*** _u_ __uu__', '~one~ ~~two~~', 'a\nb\nc', 'a  \nb\\\nc',
  '- a\n- b\n- c', '1. a\n2. b', '3. a\n4. b', '- a\n\n- b', '- a\n  - b\n    - c\n- d', '- a\n\n  para\n- b',
  '1) x\n2) y', '- [ ] task\n- [x] done', '-\n- a', '* a\n+ b\n- c', '- # heading in item', '- ```\n  code\n  ```',
  '> quote\n> more', '> a\n>\n> b', '>\n', '> - list\n> - in quote', '> > nested',
  '```\ncode <b>\n```', '```js\nlet a = 1;\n```', '```js title="x"\ny\n```', '~~~\ntilde\n~~~', '    indented\n    code',
  '`code` and ``co`de``', '***', '---', '___', 'a\n***\nb',
  '| a | b |\n| --- | :-: |\n| 1 | 2 |', '| a | b |\n| :-- | --: |', '|a|\n|-|\n|`x`|\n|*y*|', 'a | b\n--|--\n1 | 2\n3',
  '[link](http://example.com)', '[link](http://example.com "title")', '[t](<a b>)', '[t](a%20b) [u](a b)',
  '[t](https://例え.jp/パス?q=値#frag)', '[t](mailto:a@b.c)', '[t](javascript:alert(1))', '[**b**](JavaScript:x)',
  '[t](data:image/png;base64,AA)', '[t](data:text/html;base64,AA)', '[t](vbscript:x)', '[t](file:///etc/passwd)',
  '<http://example.com/a?b=c>', '<https://例え.jp/%E3%81%82>', '<a@example.com>', '<javascript:alert(1)>',
  '![alt](img.png)', '![alt *em* `code`](img.png "t")', '![a](javascript:x)', '![](data:image/gif;base64,AA)',
  '[ref]\n\n[ref]: http://example.com "T"', '[ref][]\n\n[ref]: /url', '[x][y]\n\n[y]: <a b>',
  'x<script>alert(1)</script>y', '<script>alert(1)</script>', '<div>\nblock\n</div>', 'a <b>bold</b> c',
  '&amp; &lt; &copy; &#x41; &#65; &bogus;', '\\*not em\\* \\# \\[', 'a & b < c > d "e"',
  'ページの交換手順です。 [1] [2]\n\n月に一度点検します。 [3]', '**アーカイブ済み**\n\n[アーカイブで読む](../Archives/Projects/a.html)\n\n本文 [1]',
  '[相対](./ページ/下.html) [上](../a.html) [?](?page=a%2Fb)', '[t](http://a.com:8080/p) [u](http://user:pw@a.com/)',
  '[t](//cdn.example.com/x) [u](#frag) [v](/abs/path)', '[t](HTTP://EXAMPLE.COM/A) [u](http://[::1]:80/)',
  'a\n\n\n\nb', '  leading spaces\n\ttab', 'line with trailing spaces   ', '😀 絵文字 [😀](http://e.com/😀)',
  '1. one\n\n   two\n\n2. three', '- a\n- b\n\n\n- c', '10. ten\n11. eleven', '* * *', '#notheading', '#\tTab heading',
  '```\ncode', '```\ncode\n```', '- a\n\n      code', '    a\n\n    b', '> ```\n> x', '- > q\n- b',
  '| a |\n| - |\n| <b> |', '[a](b) and [c]', '**[strong link](http://x.y)**', 'auto http://example.com text',
  'a\\\nb', '[link\nbreak](u)', '![a\nb](u)', '<!-- comment -->', 'a <!-- c --> b', '<?php ?>', '<a href="x">y</a>',
  '# 交換手順 [1]\n\n- 手順1 [2]\n- 手順2 [3]\n\n| 部品 | 周期 |\n| --- | --- |\n| フィルター | 3か月 |',
];

test('renders Markdown like markdown-it', () => {
  for (const sample of samples) {
    assert.equal(KnowledgeNest.renderMarkdown(sample), markdownIt.render(sample), JSON.stringify(sample));
  }
});
