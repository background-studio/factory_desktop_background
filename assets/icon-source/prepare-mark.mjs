import { readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import sharp from 'sharp';

const directory = path.dirname(fileURLToPath(import.meta.url));
const official = await readFile(path.join(directory, 'factory-official.svg'), 'utf8');
const paths = [...official.matchAll(/<path\b[^>]*\bd="([^"]+)"[^>]*>/g)].map(match => match[1]);
if (paths.length !== 1) throw new Error('Unexpected official Factory mark structure');
// Preserve the official geometry. Only recolour the mark; no AI redraw.
const mark = `<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
  <defs><linearGradient id="ice" x1="0" y1="0" x2="0" y2="1"><stop stop-color="#F4FBFF"/><stop offset="1" stop-color="#FFE6D5"/></linearGradient></defs>
  <g transform="translate(-94 -92) scale(2.4)">
    <path d="${paths[0]}" fill="url(#ice)" stroke="#8D371D" stroke-width="2.4" stroke-linejoin="round" paint-order="stroke"/>
    <path d="${paths[0]}" fill="url(#ice)" stroke="#FF9B6E" stroke-width="0.85" stroke-linejoin="round" paint-order="stroke"/>
  </g>
</svg>`;
await writeFile(path.join(directory, 'mark-styled.svg'), mark);
await sharp(Buffer.from(mark)).png().toFile(path.join(directory, 'mark-generated.png'));
console.log('Prepared foreground from official SVG; no generated/reconstructed geometry.');
