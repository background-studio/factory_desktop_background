import { copyFile, readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import sharp from 'sharp';

const assetDirectory = path.dirname(fileURLToPath(import.meta.url));
const configPath = path.join(assetDirectory, 'icon.config.json');
const config = JSON.parse(await readFile(configPath, 'utf8'));

const suppliedSourcePath = process.argv[2];
if (suppliedSourcePath) {
  const extension = path.extname(suppliedSourcePath).toLowerCase() || '.png';
  const name = `background-source${extension}`;
  const target = path.join(assetDirectory, name);
  if (path.resolve(suppliedSourcePath) !== target) {
    await copyFile(suppliedSourcePath, target);
  }
  config.background = name;
  await writeFile(configPath, `${JSON.stringify(config, null, 2)}\n`);
}
if (!config.background) {
  throw new Error('Pass the square artwork path: npm run prepare-background -- <file>');
}
const sourcePath = path.join(assetDirectory, config.background);

const canvasSize = 1024;
const tileInset = 64;
const tileSize = canvasSize - tileInset * 2;
const cornerRadius = 220;
const backgroundOpacity = 0.4;

const metadata = await sharp(sourcePath).metadata();
if (metadata.width !== metadata.height) {
  throw new Error(`The artwork must already be square, got ${metadata.width}x${metadata.height}. Ask the user for the crop.`);
}

// Only alpha changes. Colour, brightness and framing stay as supplied.
const roundedMask = Buffer.from(
  `<svg width="${tileSize}" height="${tileSize}">
    <rect width="${tileSize}" height="${tileSize}" rx="${cornerRadius}" fill="white"/>
  </svg>`,
);

const resizedTile = await sharp(sourcePath)
  .rotate()
  .resize(tileSize, tileSize)
  .ensureAlpha()
  .png()
  .toBuffer();

const { data: pixels, info } = await sharp(resizedTile)
  .composite([{ input: roundedMask, blend: 'dest-in' }])
  .raw()
  .toBuffer({ resolveWithObject: true });

for (let alphaOffset = 3; alphaOffset < pixels.length; alphaOffset += 4) {
  pixels[alphaOffset] = Math.round(pixels[alphaOffset] * backgroundOpacity);
}

await sharp(pixels, { raw: info })
  .extend({
    top: tileInset,
    bottom: tileInset,
    left: tileInset,
    right: tileInset,
    background: { r: 0, g: 0, b: 0, alpha: 0 },
  })
  .png()
  .toFile(path.join(assetDirectory, 'background-40.png'));

console.log(`Prepared background-40.png from ${config.background}: colours unchanged, maximum alpha 102/255 (40%).`);
