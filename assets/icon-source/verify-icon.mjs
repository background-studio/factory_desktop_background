import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import sharp from 'sharp';

const assetDirectory = path.dirname(fileURLToPath(import.meta.url));
const pluginDirectory = path.resolve(assetDirectory, '../..');
const config = JSON.parse(await readFile(path.join(assetDirectory, 'icon.config.json'), 'utf8'));

// Transparent padding must preserve every supplied pixel, not merely a crop.
const original = await sharp(path.join(assetDirectory, 'background-original.png')).ensureAlpha().raw().toBuffer({ resolveWithObject: true });
const square = await sharp(path.join(assetDirectory, config.background)).ensureAlpha().raw().toBuffer({ resolveWithObject: true });
assert.equal(square.info.width, 858);
assert.equal(square.info.height, 858);
assert.equal(original.info.width, 858);
assert.equal(original.info.height, 784);
for (let row = 0; row < 858; row += 1) {
  const line = square.data.subarray(row * 858 * 4, (row + 1) * 858 * 4);
  if (row >= 37 && row < 821) {
    assert.deepEqual(line, original.data.subarray((row - 37) * 858 * 4, (row - 36) * 858 * 4));
  } else {
    for (let offset = 3; offset < line.length; offset += 4) assert.equal(line[offset], 0);
  }
}

async function readPixels(filePath) {
  return sharp(filePath).ensureAlpha().raw().toBuffer({ resolveWithObject: true });
}

const alphaAt = (image, column, row) => image.data[(row * image.info.width + column) * 4 + 3];

const background = await readPixels(path.join(assetDirectory, 'background-40.png'));
const foreground = await readPixels(path.join(assetDirectory, 'mark-transparent.png'));
const composite = await readPixels(path.join(pluginDirectory, 'assets/icon.png'));
for (const image of [background, foreground, composite]) {
  assert.equal(image.info.width, 1024);
  assert.equal(image.info.height, 1024);
  assert.equal(alphaAt(image, 0, 0), 0, 'Outside corners must be transparent.');
}

let maximumBackgroundAlpha = 0;
for (let offset = 3; offset < background.data.length; offset += 4) {
  maximumBackgroundAlpha = Math.max(maximumBackgroundAlpha, background.data[offset]);
}
assert.equal(maximumBackgroundAlpha, 102, 'Artwork opacity must be exactly 40%.');

const { data: plain } = await sharp(path.join(assetDirectory, config.background))
  .rotate().resize(896, 896).ensureAlpha().raw().toBuffer({ resolveWithObject: true });
for (const [column, row] of [[448, 448], [300, 300], [600, 250], [448, 700]]) {
  const plainOffset = (row * 896 + column) * 4;
  const layerOffset = ((row + 64) * 1024 + column + 64) * 4;
  for (let channel = 0; channel < 3; channel += 1) {
    const difference = Math.abs(plain[plainOffset + channel] - background.data[layerOffset + channel]);
    assert.ok(difference <= 2, `Artwork colour changed at (${column}, ${row}).`);
  }
}

let transparentMark = 0;
let opaqueMark = 0;
for (let offset = 3; offset < foreground.data.length; offset += 4) {
  if (foreground.data[offset] === 0) transparentMark += 1;
  if (foreground.data[offset] === 255) opaqueMark += 1;
}
const total = foreground.data.length / 4;
assert.ok(transparentMark > total * 0.3, 'Mark background was not removed; set removeWhiteBackground or regenerate with alpha.');
assert.ok(opaqueMark > total * 0.03, 'Mark has too few opaque pixels to read at 16px.');

const windowsIcon = await readFile(path.join(pluginDirectory, 'src-tauri/icons/icon.ico'));
const expectedDimensions = [16, 24, 32, 48, 64, 128, 256];
assert.equal(windowsIcon.readUInt16LE(4), expectedDimensions.length);
for (const [imageIndex, dimension] of expectedDimensions.entries()) {
  const entryOffset = 6 + imageIndex * 16;
  const length = windowsIcon.readUInt32LE(entryOffset + 8);
  const start = windowsIcon.readUInt32LE(entryOffset + 12);
  const metadata = await sharp(windowsIcon.subarray(start, start + length)).metadata();
  assert.equal(metadata.width, dimension);
  assert.equal(metadata.hasAlpha, true);
}

if (process.argv[2]) {
  const hostDirectory = path.resolve(process.argv[2]);
  const bundled = await readFile(path.join(hostDirectory, `src-tauri/resources/${config.pluginId}.png`));
  const published = await readFile(path.join(hostDirectory, `public/plugins/${config.pluginId}.png`));
  assert.deepEqual(bundled, published);
  const catalog = JSON.parse(await readFile(path.join(hostDirectory, 'src-tauri/resources/catalog.json'), 'utf8'));
  const entry = catalog.plugins.find((plugin) => plugin.id === config.pluginId);
  assert.ok(entry, `catalog.json has no ${config.pluginId} entry.`);
  assert.equal(entry.icon, `${config.pluginId}.png`, 'catalog.json entry must reference the bundled icon.');
  const catalogSource = await readFile(path.join(hostDirectory, 'src-tauri/src/catalog.rs'), 'utf8');
  assert.ok(catalogSource.includes(`resources/${config.pluginId}.png`), 'sync_bundled_icons must include the icon.');
}

console.log('PASS: 40% artwork with original colours, transparent corners, readable mark, seven ICO sizes.');
