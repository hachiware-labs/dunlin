import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { mkdtemp, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { deflateSync } from 'node:zlib';

const runNode = promisify(execFile);

test('builds mixed PDF and text sources in stable order and preserves the active build on failure', async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-mixed-sources-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const projectCli = fileURLToPath(new URL('../cli.mjs', import.meta.url));
  const env = { ...process.env, DUNLIN_GLOBAL_CONFIG: join(root, 'global.yaml') };
  const json = async (...args) => JSON.parse((await runNode(process.execPath, [projectCli, ...args, '--json'],
    { cwd: root, env, timeout: 15_000 })).stdout);
  await json('init', 'rag', '.');
  const docs = join(root, 'docs');
  const pdf = textLayerPdf('Calibration procedure', 'Replace the filter every six months.');
  await Promise.all([
    writeFile(join(docs, 'a-original.pdf'), pdf),
    writeFile(join(docs, 'b-duplicate.pdf'), pdf),
    writeFile(join(docs, 'c-empty.md'), ' '),
    writeFile(join(docs, 'd-empty.txt'), ''),
    writeFile(join(docs, 'e-guide.md'), '# 点検手順\n電源を止めます。\n'),
    writeFile(join(docs, 'ignored.bin'), 'unsupported'),
  ]);
  const build = await json('build', '--skip-unsupported');
  assert.equal(build.data.documents, 4);
  assert.deepEqual(build.warnings.filter(({ code }) => code === 'EMPTY_EXTRACTION').map(({ target }) => target),
    ['docs:c-empty.md', 'docs:d-empty.txt']);
  const search = (await json('search', 'Calibration')).data;
  assert.deepEqual(search.results.map(({ document_id }) => document_id).sort(), ['docs:a-original.pdf', 'docs:b-duplicate.pdf']);
  assert.ok(search.results.every(({ page }) => page === 1));
  const originals = join(root, '.dunlin', 'artifacts', 'originals', 'shared');
  const archived = await Promise.all((await readdir(originals)).map((name) => readFile(join(originals, name))));
  assert.equal(archived.filter((bytes) => bytes.equals(pdf)).length, 1);
  const repeated = await json('build', '--skip-unsupported');
  assert.equal(repeated.data.reused, true);
  for (const [stage, result] of Object.entries(repeated.data.stages)) {
    assert.equal(result.id, build.data.stages[stage].id);
    assert.equal(result.reused, true);
    assert.ok(Number.isInteger(result.elapsed_ms) && result.elapsed_ms >= 0);
  }

  await writeFile(join(docs, 'a-broken.pdf'), '%PDF-1.4\ninvalid');
  await rm(join(docs, 'b-duplicate.pdf'));
  await writeFile(join(docs, 'e-guide.md'), '# 点検手順\n新しい手順です。\n');
  const changes = (await json('build', '--dry-run')).data.files.map(({ path, change }) => [path, change]);
  assert.deepEqual(changes.filter(([, change]) => change !== 'unchanged'),
    [['a-broken.pdf', 'added'], ['e-guide.md', 'updated'], ['ignored.bin', 'added'], ['b-duplicate.pdf', 'deleted']]);
  await assert.rejects(json('build', '--skip-unsupported'), (failure) => {
    assert.equal(failure.code, 1);
    assert.match(JSON.parse(failure.stdout).errors[0].message, /Could not load docs:a-broken\.pdf/u);
    return true;
  });
  assert.deepEqual((await json('search', 'Calibration')).data, search);
});

test('uses text-layer PDFs in an OCR-enabled project and guides setup for scans', async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'dunlin-project-ocr-'));
  t.after(() => rm(root, { recursive: true, force: true, maxRetries: 5 }));
  const projectCli = fileURLToPath(new URL('../cli.mjs', import.meta.url));
  const env = { ...process.env, DUNLIN_GLOBAL_CONFIG: join(root, 'global.yaml'), DUNLIN_OCR_HOME: join(root, 'ocr') };
  const run = (...args) => runNode(process.execPath, [projectCli, ...args], { cwd: root, env, timeout: 15_000 });
  await run('config', 'set', 'ocr');
  await run('init', 'rag', '.');
  await writeFile(join(root, 'docs', 'manual.pdf'), textLayerPdf('Calibration procedure'));
  await run('build');
  const { stdout } = await run('search', 'Calibration', '--json');
  const [result] = JSON.parse(stdout).data.results;
  assert.equal(result.document_id, 'docs:manual.pdf');
  assert.equal(result.page, 1);

  await writeFile(join(root, 'docs', 'scan.pdf'), scannedPdf(1));
  await assert.rejects(run('build'), { code: 1, stderr: /Run dunlin add ocr\./u });
});

/** A PDF whose pages are images without a text layer, as scanners write them. */
function scannedPdf(pages) {
  const width = 720;
  const height = 360;
  const objects = ['<< /Type /Catalog /Pages 2 0 R >>',
    `<< /Type /Pages /Count ${pages} /Kids [${Array.from({ length: pages }, (_, index) => `${3 + 3 * index} 0 R`).join(' ')}] >>`];
  for (let page = 0; page < pages; page++) {
    // A white page with one dark bar where a line of text would be.
    const rgb = Buffer.alloc(width * height * 3, 255);
    for (let y = 140; y < 180; y++) rgb.fill(0, (y * width + 80) * 3, (y * width + 400) * 3);
    const image = deflateSync(rgb);
    const index = objects.length + 1;
    objects.push(`<< /Type /Page /Parent 2 0 R /MediaBox [0 0 ${width} ${height}] /Resources << /XObject << /Im ${index + 1} 0 R >> >> /Contents ${index + 2} 0 R >>`,
      Buffer.concat([Buffer.from(`<< /Type /XObject /Subtype /Image /Width ${width} /Height ${height} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode /Length ${image.length} >>\nstream\n`), image, Buffer.from('\nendstream')]),
      `<< /Length ${String(`q ${width} 0 0 ${height} 0 0 cm /Im Do Q`).length} >>\nstream\nq ${width} 0 0 ${height} 0 0 cm /Im Do Q\nendstream`);
  }
  return pdfWithObjects(objects);
}

function textLayerPdf(title, body) {
  const stream = `BT /F1 24 Tf 72 700 Td (${title}) Tj ET`
    + (body ? `\nBT /F1 12 Tf 72 650 Td (${body}) Tj ET` : '');
  return pdfWithObjects([
    '<< /Type /Catalog /Pages 2 0 R >>', '<< /Type /Pages /Count 1 /Kids [3 0 R] >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>',
    '<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>', `<< /Length ${stream.length} >>\nstream\n${stream}\nendstream`,
  ]);
}

function pdfWithObjects(objects) {
  const pdf = [Buffer.from('%PDF-1.4\n')];
  const offsets = [];
  let offset = pdf[0].length;
  for (const [index, object] of objects.entries()) {
    offsets.push(offset);
    const chunk = Buffer.concat([Buffer.from(`${index + 1} 0 obj\n`), Buffer.from(object), Buffer.from('\nendobj\n')]);
    pdf.push(chunk);
    offset += chunk.length;
  }
  pdf.push(Buffer.from(`xref\n0 ${objects.length + 1}\n0000000000 65535 f \n${offsets.map((value) => `${String(value).padStart(10, '0')} 00000 n \n`).join('')}trailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\nstartxref\n${offset}\n%%EOF\n`));
  return Buffer.concat(pdf);
}
