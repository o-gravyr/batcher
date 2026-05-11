// Generate a 1024×1024 placeholder app icon (black rounded square with a
// three-bar "batch" glyph). Pure Node — no external deps. Writes to argv[2]
// or `logo.png` at the project root. After running this, run
// `npx tauri icon logo.png` to fan out into the platform icon set.
import { createWriteStream } from "node:fs";
import { deflateSync } from "node:zlib";
import { Buffer } from "node:buffer";

const SIZE = 1024;
const RADIUS = 220;
const BG = [17, 17, 17, 255];   // #111111
const FG = [255, 255, 255, 255]; // white

function inRoundedSquare(x, y, size, radius) {
  if (x >= radius && x <= size - 1 - radius) return y >= 0 && y <= size - 1;
  if (y >= radius && y <= size - 1 - radius) return x >= 0 && x <= size - 1;
  for (const [cx, cy] of [
    [radius, radius],
    [size - 1 - radius, radius],
    [radius, size - 1 - radius],
    [size - 1 - radius, size - 1 - radius],
  ]) {
    if (
      (x < radius && y < radius && cx === radius && cy === radius) ||
      (x > size - 1 - radius && y < radius && cx === size - 1 - radius && cy === radius) ||
      (x < radius && y > size - 1 - radius && cx === radius && cy === size - 1 - radius) ||
      (x > size - 1 - radius && y > size - 1 - radius && cx === size - 1 - radius && cy === size - 1 - radius)
    ) {
      const dx = x - cx;
      const dy = y - cy;
      return dx * dx + dy * dy <= radius * radius;
    }
  }
  return true;
}

// Three stacked horizontal bars, slightly stair-stepped left-to-right —
// suggests "batch" / list of items.
function inGlyph(x, y) {
  const cx = SIZE / 2;
  const cy = SIZE / 2;
  const barH = 84;
  const gap = 80;
  const widths = [520, 440, 360];
  const startY = cy - (barH * 3 + gap * 2) / 2;
  for (let i = 0; i < 3; i++) {
    const yTop = startY + i * (barH + gap);
    const yBot = yTop + barH;
    const w = widths[i];
    const xLeft = cx - w / 2 - (40 - i * 20); // shift left by less each row
    const xRight = xLeft + w;
    if (x >= xLeft && x <= xRight && y >= yTop && y <= yBot) {
      // rounded ends
      const rad = barH / 2;
      const yMid = (yTop + yBot) / 2;
      if (x < xLeft + rad) {
        const dx = x - (xLeft + rad);
        const dy = y - yMid;
        return dx * dx + dy * dy <= rad * rad;
      }
      if (x > xRight - rad) {
        const dx = x - (xRight - rad);
        const dy = y - yMid;
        return dx * dx + dy * dy <= rad * rad;
      }
      return true;
    }
  }
  return false;
}

function drawPixel(buf, x, y, color) {
  if (x < 0 || y < 0 || x >= SIZE || y >= SIZE) return;
  const idx = (y * SIZE + x) * 4;
  buf[idx] = color[0];
  buf[idx + 1] = color[1];
  buf[idx + 2] = color[2];
  buf[idx + 3] = color[3];
}

const pixels = Buffer.alloc(SIZE * SIZE * 4, 0);
for (let y = 0; y < SIZE; y++) {
  for (let x = 0; x < SIZE; x++) {
    if (!inRoundedSquare(x, y, SIZE, RADIUS)) continue;
    if (inGlyph(x, y)) drawPixel(pixels, x, y, FG);
    else drawPixel(pixels, x, y, BG);
  }
}

function crc32(buf) {
  let c;
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[n] = c >>> 0;
  }
  let crc = 0xffffffff;
  for (let i = 0; i < buf.length; i++) crc = table[(crc ^ buf[i]) & 0xff] ^ (crc >>> 8);
  return (crc ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length, 0);
  const typeBuf = Buffer.from(type, "ascii");
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(Buffer.concat([typeBuf, data])), 0);
  return Buffer.concat([len, typeBuf, data, crc]);
}

const SIGNATURE = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(SIZE, 0);
ihdr.writeUInt32BE(SIZE, 4);
ihdr[8] = 8;
ihdr[9] = 6;
ihdr[10] = 0;
ihdr[11] = 0;
ihdr[12] = 0;

const stride = SIZE * 4;
const raw = Buffer.alloc((stride + 1) * SIZE);
for (let y = 0; y < SIZE; y++) {
  raw[y * (stride + 1)] = 0;
  pixels.copy(raw, y * (stride + 1) + 1, y * stride, (y + 1) * stride);
}
const idat = deflateSync(raw, { level: 9 });

const png = Buffer.concat([
  SIGNATURE,
  chunk("IHDR", ihdr),
  chunk("IDAT", idat),
  chunk("IEND", Buffer.alloc(0)),
]);

const outPath = process.argv[2] || "logo.png";
const stream = createWriteStream(outPath);
stream.write(png);
stream.end();
stream.on("close", () => console.log(`✓ wrote ${outPath} (${png.length} bytes, ${SIZE}×${SIZE})`));
