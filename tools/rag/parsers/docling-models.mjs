import { join } from 'node:path';
import { download, downloadVerified } from './download.mjs';

const release = 'https://github.com/docling-project/docling.rs/releases/download/models-v1';
const models = {
  DOCLING_LAYOUT_ONNX: 'layout_heron.onnx',
  DOCLING_TABLEFORMER_ENCODER: 'tableformer/encoder.onnx',
  DOCLING_TABLEFORMER_DECODER: 'tableformer/decoder.onnx',
  DOCLING_TABLEFORMER_BBOX: 'tableformer/bbox.onnx',
};
const assets = [...Object.values(models), 'tableformer/decoder.onnx.data', 'tableformer/bbox.onnx.data'];

export function configureDoclingModels(dir) {
  if (!dir) return;
  for (const [name, file] of Object.entries(models)) process.env[name] = join(dir, file);
}

export async function installDoclingModels(dir) {
  const manifest = await (await download(`${release}/SHA256SUMS`)).text();
  const checksums = new Map(manifest.trim().split(/\r?\n/u).map((line) => line.trim().split(/\s+/u).reverse()));
  const results = await Promise.allSettled(assets.map(async (asset) => {
    const name = asset.split('/').at(-1);
    const checksum = checksums.get(name);
    if (!/^[a-f0-9]{64}$/u.test(checksum)) throw new Error(`Docling model checksum missing for ${name}`);
    await downloadVerified(`${release}/${name}`, join(dir, asset), checksum);
  }));
  const failed = results.find(({ status }) => status === 'rejected');
  if (failed) throw failed.reason;
}

