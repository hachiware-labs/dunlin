import { dirname } from 'node:path';
import { createInterface } from 'node:readline';

import { newConfigPath } from './config.mjs';

const choices = [
  ['para', 'PARA', '資料を分類して出典付きノートを作る'],
  ['llm-wiki', 'LLM Wiki', '文書を引用した Wiki を作る'],
  ['rag', 'RAG', '資料を検索し、質問に出典付きで答える'],
  ['mcp', '検索専用 MCP', 'MCP クライアントから資料を検索する'],
];

export async function promptInitPreset(directory, { input = process.stdin, output = process.stderr } = {}) {
  const path = await newConfigPath(directory);
  const terminal = createInterface({ input, output });
  const lines = terminal[Symbol.asyncIterator]();
  let closed = false;
  terminal.once('close', () => { closed = true; });
  const ask = async (prompt) => {
    if (closed) {
      output.write(prompt);
    } else {
      terminal.setPrompt(prompt);
      terminal.prompt();
    }
    return (await lines.next()).value?.trim();
  };

  try {
    output.write('Dunlin の用途を選んでください。\n');
    for (const [index, [, label, description]] of choices.entries()) {
      output.write(`  ${index + 1}. ${label} — ${description}\n`);
    }
    let selection;
    while (!selection) {
      const answer = await ask(`用途 [1-${choices.length}] `);
      if (answer === undefined) return null;
      selection = choices.find((_, index) => answer === String(index + 1));
      if (!selection) output.write(`1〜${choices.length} の番号を入力してください。\n`);
    }

    const [preset, label] = selection;
    output.write(`作業フォルダ ${dirname(path)}\nプリセット ${label}\n`);
    while (true) {
      const answer = (await ask('ここに dunlin.json と docs/ を作成しますか？ [Y/n] '))?.toLowerCase();
      if (answer === '' || answer === 'y' || answer === 'yes') return preset;
      if (answer === undefined || answer === 'n' || answer === 'no') return null;
      output.write('Enter で作成、n で中止します。\n');
    }
  } finally {
    terminal.close();
  }
}
