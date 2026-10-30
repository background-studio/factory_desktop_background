import { copyFile, mkdir, readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import sharp from 'sharp';

const assetDirectory = path.dirname(fileURLToPath(import.meta.url));
const pluginDirectory = path.resolve(assetDirectory, '../..');
const iconDirectory = path.join(pluginDirectory, 'src-tauri/icons');
const hostDirectory = process.argv[2] ? path.resolve(process.argv[2]) : null;
const config = JSON.parse(await readFile(path.join(assetDirectory, 'icon.config.json'), 'utf8'));
const canvasSize = 1024;

const { data: markPixels, info: markInfo } = await sharp(path.join(assetDirectory, config.mark))
  .resize(canvasSize, canvasSize, { fit: 'contain', background: { r: 0, g: 0, b: 0, alpha: 0 } })
  .ensureAlpha()
  .raw()
  .toBuffer({ resolveWithObject: true });

if (config.removeWhiteBackground) {
  // Image generation often returns an opaque white field. Clear near-white pixels
  // connected to the canvas edge; enclosed light fills inside the mark stay.
  const nearWhite = (offset) => {
    const red = markPixels[offset];
    const green = markPixels[offset + 1];
    const blue = markPixels[offset + 2];
    return red > 246 && green > 246 && blue > 246
      && Math.max(red, green, blue) - Math.min(red, green, blue) < 14;
  };
  const seen = new Uint8Array(canvasSize * canvasSize);
  const queue = [];
  const push = (x, y) => {
    const index = y * canvasSize + x;
    if (seen[index] || !nearWhite(index * 4)) return;
    seen[index] = 1;
    queue.push(index);
  };
  for (let edge = 0; edge < canvasSize; edge += 1) {
    push(edge, 0);
    push(edge, canvasSize - 1);
    push(0, edge);
    push(canvasSize - 1, edge);
  }
  for (let cursor = 0; cursor < queue.length; cursor += 1) {
    const index = queue[cursor];
    const x = index % canvasSize;
    const y = (index - x) / canvasSize;
    if (x > 0) push(x - 1, y);
    if (x + 1 < canvasSize) push(x + 1, y);
    if (y > 0) push(x, y - 1);
    if (y + 1 < canvasSize) push(x, y + 1);
  }
  for (let index = 0; index < seen.length; index += 1) {
    if (seen[index]) markPixels.fill(0, index * 4, index * 4 + 4);
  }
}

const foregroundPath = path.join(assetDirectory, 'mark-transparent.png');
await sharp(markPixels, { raw: markInfo }).png().toFile(foregroundPath);

const { shadow, stroke, highlight } = config.frame;
const frameSource = `<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
  <rect x="74" y="74" width="876" height="876" rx="208" fill="none" stroke="${shadow}" stroke-width="22" stroke-opacity="0.35"/>
  <rect x="74" y="74" width="876" height="876" rx="208" fill="none" stroke="${stroke}" stroke-width="10" stroke-opacity="0.90"/>
  <rect x="80" y="80" width="864" height="864" rx="202" fill="none" stroke="${highlight}" stroke-width="1.5" stroke-opacity="0.40"/>
</svg>`;
await writeFile(path.join(assetDirectory, 'frame.svg'), `${frameSource}\n`);

const composedIcon = await sharp(path.join(assetDirectory, 'background-40.png'))
  .composite([{ input: Buffer.from(frameSource) }, { input: foregroundPath }])
  .png()
  .toBuffer();
const masterPath = path.join(pluginDirectory, 'assets/icon.png');
await mkdir(path.dirname(masterPath), { recursive: true });
await writeFile(masterPath, composedIcon);

for (const [theme, background] of [['dark', '#161D27'], ['light', '#F2F5F9']]) {
  await sharp(composedIcon).flatten({ background }).resize(512, 512)
    .png().toFile(path.join(assetDirectory, `preview-${theme}.png`));
}

await mkdir(iconDirectory, { recursive: true });
await sharp(composedIcon).resize(256, 256).png().toFile(path.join(iconDirectory, 'icon.png'));

const dimensions = [16, 24, 32, 48, 64, 128, 256];
const images = await Promise.all(dimensions.map((dimension) =>
  sharp(composedIcon).resize(dimension, dimension).png().toBuffer(),
));
const directory = Buffer.alloc(6 + dimensions.length * 16);
directory.writeUInt16LE(1, 2);
directory.writeUInt16LE(dimensions.length, 4);
let imageOffset = directory.length;
for (const [imageIndex, dimension] of dimensions.entries()) {
  const entryOffset = 6 + imageIndex * 16;
  directory[entryOffset] = dimension === 256 ? 0 : dimension;
  directory[entryOffset + 1] = dimension === 256 ? 0 : dimension;
  directory.writeUInt16LE(1, entryOffset + 4);
  directory.writeUInt16LE(32, entryOffset + 6);
  directory.writeUInt32LE(images[imageIndex].length, entryOffset + 8);
  directory.writeUInt32LE(imageOffset, entryOffset + 12);
  imageOffset += images[imageIndex].length;
}
await writeFile(path.join(iconDirectory, 'icon.ico'), Buffer.concat([directory, ...images]));

if (hostDirectory) {
  const hostIcon = await sharp(composedIcon).resize(512, 512).png().toBuffer();
  const resourcePath = path.join(hostDirectory, `src-tauri/resources/${config.pluginId}.png`);
  await writeFile(resourcePath, hostIcon);
  const publicDirectory = path.join(hostDirectory, 'public/plugins');
  await mkdir(publicDirectory, { recursive: true });
  await copyFile(resourcePath, path.join(publicDirectory, `${config.pluginId}.png`));
}

console.log(`Composed ${masterPath} and exported icon.png plus ICO sizes ${dimensions.join(', ')}.`);
