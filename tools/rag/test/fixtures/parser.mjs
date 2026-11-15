// Test parser for loader.command. PARSER_MODE selects the output; PARSER_COUNTER records each call.
import { appendFileSync, readFileSync } from 'node:fs';

const [input] = process.argv.slice(2);
const mode = process.env.PARSER_MODE ?? 'markdown';
if (process.env.PARSER_COUNTER) appendFileSync(process.env.PARSER_COUNTER, mode + ' ' + (input ? 'file' : 'stdin') + '\n');
if (mode === 'version') {
  process.stdout.write('fixture-parser ' + (process.env.PARSER_VERSION ?? '1.0') + '\n');
  process.exit(0);
}
if (mode === 'fail') {
  process.stderr.write('fixture could not read the drawing\n');
  process.exit(3);
}
if (mode === 'sleep') setTimeout(() => {}, 30_000);
else {
  const chunks = [];
  if (input) chunks.push(readFileSync(input));
  else for await (const chunk of process.stdin) chunks.push(chunk);
  const text = Buffer.concat(chunks).toString('utf8').trim();
  if (mode === 'json') {
    process.stdout.write(JSON.stringify({ content: '# 図面\n' + text + '\n', format: 'markdown',
      locations: [{ page: 1, range: { startLine: 1, endLine: 2 } }] }));
  } else if (mode === 'bad-json') {
    process.stdout.write(JSON.stringify({ content: text, format: 'markdown', extra: true }));
  } else {
    process.stdout.write('# 図面\n' + text + '\n');
  }
}
