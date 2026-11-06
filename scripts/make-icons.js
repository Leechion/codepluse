'use strict';
// Generates the PWA/desktop icons (public/icon-*.png) with zero dependencies:
// a rounded indigo gradient tile with the white "H" mark, PNG-encoded by hand.
// Re-run with `node scripts/make-icons.js` after changing the design.
const zlib = require('node:zlib');
const fs = require('node:fs');
const path = require('node:path');

let CRC_TABLE = null;
function crc32(buf) {
  if (!CRC_TABLE) {
    CRC_TABLE = new Int32Array(256);
    for (let n = 0; n < 256; n++) { let c = n; for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1; CRC_TABLE[n] = c; }
  }
  let c = -1;
  for (const b of buf) c = CRC_TABLE[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ -1) >>> 0;
}
function chunk(type, data) {
  const len = Buffer.alloc(4); len.writeUInt32BE(data.length);
  const t = Buffer.from(type, 'ascii');
  const crc = Buffer.alloc(4); crc.writeUInt32BE(crc32(Buffer.concat([t, data])));
  return Buffer.concat([len, t, data, crc]);
}
function encode(size, px) {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0); ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8; ihdr[9] = 6; // 8-bit RGBA
  const raw = Buffer.alloc(size * (size * 4 + 1));
  for (let y = 0; y < size; y++) {
    raw[y * (size * 4 + 1)] = 0; // filter: none
    px.copy(raw, y * (size * 4 + 1) + 1, y * size * 4, (y + 1) * size * 4);
  }
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', ihdr), chunk('IDAT', zlib.deflateSync(raw, { level: 9 })), chunk('IEND', Buffer.alloc(0))
  ]);
}
function draw(size, margin) {
  const px = Buffer.alloc(size * size * 4);
  const put = (x, y, r, g, b, a) => {
    if (x < 0 || y < 0 || x >= size || y >= size) return;
    const i = (y * size + x) * 4;
    px[i] = r; px[i + 1] = g; px[i + 2] = b; px[i + 3] = a;
  };
  const x0 = size * margin, x1 = size * (1 - margin), y0 = x0, y1 = x1;
  const rad = (x1 - x0) * 0.22;
  for (let y = 0; y < size; y++) for (let x = 0; x < size; x++) {
    const dx = Math.max(x0 + rad - x, 0, x - (x1 - rad));
    const dy = Math.max(y0 + rad - y, 0, y - (y1 - rad));
    if (dx * dx + dy * dy > rad * rad) { put(x, y, 0, 0, 0, 0); continue; }
    const t = (y - y0) / (y1 - y0); // 0x6d5cf0 -> 0x4a5ce0, same as the in-app avatar gradient
    put(x, y, Math.round(0x6d + (0x4a - 0x6d) * t), 0x5c, Math.round(0xf0 + (0xe0 - 0xf0) * t), 255);
  }
  // White H mark: two bars and a crossbar.
  const bw = Math.max(2, Math.round(size * 0.075));
  const gh = Math.round(size * 0.40);
  const gap = Math.round(size * 0.11);
  const cy = size / 2;
  const yA = Math.round(cy - gh / 2), yB = Math.round(cy + gh / 2);
  const lx = Math.round(size / 2 - (bw * 2 + gap) / 2);
  const fill = (xa, xb, ya, yb) => { for (let y = ya; y < yb; y++) for (let x = xa; x < xb; x++) put(x, y, 255, 255, 255, 255); };
  fill(lx, lx + bw, yA, yB);
  fill(lx + bw + gap, lx + bw * 2 + gap, yA, yB);
  const midY = Math.round((yA + yB) / 2 - bw / 2);
  fill(lx, lx + bw * 2 + gap, midY, midY + bw);
  return px;
}
const out = path.resolve(__dirname, '../public');
fs.mkdirSync(out, { recursive: true });
for (const [file, size, margin] of [['icon-192.png', 192, 0.06], ['icon-512.png', 512, 0.06], ['icon-512-maskable.png', 512, 0.16]]) {
  fs.writeFileSync(path.join(out, file), encode(size, draw(size, margin)));
  console.log('written', file);
}
