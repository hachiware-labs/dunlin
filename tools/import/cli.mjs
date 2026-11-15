import { parseArgs } from 'node:util';

import { importDocuments } from './import-documents.mjs';

const usage = 'Usage: node tools/import/cli.mjs --index <directory> --source <id> [--corpus-backend <sqlite|json>] [--keep-original] [--s3-endpoint <url>] <url>...';

try {
  const { values, positionals } = parseArgs({
    options: {
      index: { type: 'string' },
      source: { type: 'string' },
      'corpus-backend': { type: 'string' },
      'keep-original': { type: 'boolean' },
      's3-endpoint': { type: 'string' },
      help: { type: 'boolean', short: 'h' },
    },
    allowPositionals: true,
  });
  if (values.help) {
    console.log(usage);
  } else {
    const corpusBackend = values['corpus-backend'];
    if (!values.index || !/^[a-zA-Z0-9_.-]{1,64}$/u.test(values.source ?? '') || positionals.length === 0
      || (corpusBackend !== undefined && !['sqlite', 'json'].includes(corpusBackend))) {
      throw new Error(usage);
    }
    const result = await importDocuments(values.index, values.source, positionals, values['s3-endpoint'], { corpusBackend, keepOriginal: values['keep-original'] });
    console.log(JSON.stringify(result));
  }
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
