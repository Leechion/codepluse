'use strict';
// Generates the PWA/desktop icons (public/icon-*.png + public/codepluse.ico)
// with zero dependencies. Design: a rounded indigo gradient tile (brand
// gradient #6d5cf0 -> #4a5ce0) with a white hub-and-spoke mark — a central
// hub ring carrying a plus (codepluse), three spokes radiating to three
// agent nodes. Geometry is SDF-based with supersampled coverage for clean
// anti-aliasing at every size. Re-run `node scripts/make-icons.js` after
// changing the design.
const zlib = require('node:zlib');
const fs = require('node:fs');
const path = require('node:path');

// ── PNG encoder (hand-rolled, same as the original pipeline) ──
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

// ── geometry: signed distances ──
// Canonical rounded-box SDF (negative inside). cx/cy = center; halfW/halfH =
// half extents; rad = corner radius.
function sdRoundRect(px, py, cx, cy, halfW, halfH, rad) {
  const qx = Math.abs(px - cx) - (halfW - rad);
  const qy = Math.abs(py - cy) - (halfH - rad);
  return Math.hypot(Math.max(qx, 0), Math.max(qy, 0)) + Math.min(Math.max(qx, qy), 0) - rad;
}
function sdCircle(px, py, cx, cy, r) {
  return Math.hypot(px - cx, py - cy) - r;
}
// distance to a capsule (segment with radius)
function sdCapsule(px, py, ax, ay, bx, by, r) {
  const pax = px - ax, pay = py - ay, bax = bx - ax, bay = by - ay;
  const t = Math.max(0, Math.min(1, (pax * bax + pay * bay) / (bax * bax + bay * bay)));
  return Math.hypot(pax - bax * t, pay - bay * t) - r;
}
function sdPlus(px, py, cx, cy, arm, thick) {
  // plus = union of two capsules through the center
  const v = sdCapsule(px, py, cx, cy - arm, cx, cy + arm, thick);
  const h = sdCapsule(px, py, cx - arm, cy, cx + arm, cy, thick);
  return Math.min(v, h);
}
const smooth = (d, softness) => Math.max(0, Math.min(1, 0.5 - d / softness));

// ── design ──
// tile: rounded square, vertical gradient (brand). glyph: hub ring with a
// gradient plus in the middle, three spokes to three satellite nodes.
function draw(size, margin, opts = {}) {
  const SS = opts.ss ?? 3; // supersample factor per axis
  const px = Buffer.alloc(size * size * 4);
  const put = (x, y, r, g, b, a) => {
    const i = (y * size + x) * 4;
    px[i] = Math.min(255, Math.round(px[i] + r * a));
    px[i + 1] = Math.min(255, Math.round(px[i + 1] + g * a));
    px[i + 2] = Math.min(255, Math.round(px[i + 2] + b * a));
    px[i + 3] = Math.min(255, Math.round(px[i + 3] + a * 255));
  };

  const x0 = size * margin, x1 = size * (1 - margin), y0 = x0, y1 = x1;
  const rad = (x1 - x0) * 0.225;
  const cx = size / 2, cy = size / 2;
  const hubR = size * 0.135;         // hub outer radius
  const hubRing = size * 0.038;      // ring thickness (white band)
  const plusArm = hubR * 0.52;       // plus arm length (half-extent)
  const plusThick = size * 0.028;
  const nodeR = size * 0.058;        // satellite node radius
  const spokeW = size * 0.030;       // spoke line width (radius of capsule)
  const spokeLen = size * 0.30;      // hub center -> node center
  const angles = [-90, 30, 150].map((a) => (a * Math.PI) / 180);
  const gradientAt = (y) => {
    const t = Math.max(0, Math.min(1, (y - y0) / (y1 - y0)));
    return [Math.round(0x6d + (0x4a - 0x6d) * t), 0x5c, Math.round(0xf0 + (0xe0 - 0xf0) * t)];
  };

  const soft = Math.max(1.2, size * 0.004); // AA band in device pixels (~1-2px)

  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      // per-pixel supersampled coverage of glyph over tile
      let tileCov = 0, glyphCov = 0;
      let tileR = 0, tileG = 0, tileB = 0;
      for (let sy = 0; sy < SS; sy++) for (let sx = 0; sx < SS; sx++) {
        const sxp = x + (sx + 0.5) / SS;
        const syp = y + (sy + 0.5) / SS;
        const t = smooth(sdRoundRect(sxp, syp, size / 2, size / 2, (x1 - x0) / 2, (y1 - y0) / 2, rad), soft);
        if (t <= 0) continue;
        tileCov += t;
        const [r, g, b] = gradientAt(syp);
        tileR += r * t; tileG += g * t; tileB += b * t;
        // glyph mask: hub ring, spokes, nodes, hub disc with plus cut
        let d = sdCircle(sxp, syp, cx, cy, hubR);
        d = Math.max(d, -sdCircle(sxp, syp, cx, cy, hubR - hubRing)); // ring
        for (const a of angles) {
          const nx = cx + Math.cos(a) * spokeLen, ny = cy + Math.sin(a) * spokeLen;
          d = Math.min(d, sdCapsule(sxp, syp, cx + Math.cos(a) * hubR * 0.9, cy + Math.sin(a) * hubR * 0.9, nx, ny, spokeW));
          d = Math.min(d, sdCircle(sxp, syp, nx, ny, nodeR));
        }
        glyphCov += smooth(d, soft);
      }
      const samples = SS * SS;
      if (tileCov <= 0) continue; // outside tile, fully transparent
      const tc = tileCov / samples;
      tileR /= tileCov; tileG /= tileCov; tileB /= tileCov;
      const gc = glyphCov / samples;
      // composite: glyph (white) over tile, both over transparent
      const r = tileR * (1 - gc) + 255 * gc;
      const g = tileG * (1 - gc) + 255 * gc;
      const b = tileB * (1 - gc) + 255 * gc;
      put(x, y, r, g, b, tc);
    }
  }

  // the plus inside the hub: punch it back to the gradient color so it reads
  // as a cut-out of the white ring disc — drawn in a second pass for crispness.
  const midColor = gradientAt(cy);
  for (let y = Math.floor(cy - hubR - 2); y <= Math.ceil(cy + hubR + 2); y++) {
    for (let x = Math.floor(cx - hubR - 2); x <= Math.ceil(cx + hubR + 2); x++) {
      let cov = 0;
      for (let sy = 0; sy < SS; sy++) for (let sx = 0; sx < SS; sx++) {
        const sxp = x + (sx + 0.5) / SS, syp = y + (sy + 0.5) / SS;
        cov += smooth(sdPlus(sxp, syp, cx, cy, plusArm, plusThick), soft) / (SS * SS);
      }
      if (cov <= 0) continue;
      const i = (y * size + x) * 4;
      if (px[i + 3] === 0) continue; // outside tile
      // blend the gradient color over the existing white hub disc
      const a = cov;
      px[i] = Math.round(px[i] * (1 - a) + midColor[0] * a);
      px[i + 1] = Math.round(px[i + 1] * (1 - a) + midColor[1] * a);
      px[i + 2] = Math.round(px[i + 2] * (1 - a) + midColor[2] * a);
    }
  }
  return px;
}

// ── ICO writer: embeds downsampled PNG entries (16..256), Vista+ compatible ──
function downsample(size, src, srcSize) {
  const px = Buffer.alloc(size * size * 4);
  const scale = srcSize / size;
  for (let y = 0; y < size; y++) for (let x = 0; x < size; x++) {
    // box sample the source
    let r = 0, g = 0, b = 0, a = 0, n = 0;
    const xa = Math.floor(x * scale), xb = Math.min(srcSize, Math.ceil((x + 1) * scale));
    const ya = Math.floor(y * scale), yb = Math.min(srcSize, Math.ceil((y + 1) * scale));
    for (let yy = ya; yy < yb; yy++) for (let xx = xa; xx < xb; xx++) {
      const i = (yy * srcSize + xx) * 4;
      r += src[i]; g += src[i + 1]; b += src[i + 2]; a += src[i + 3]; n++;
    }
    const o = (y * size + x) * 4;
    px[o] = r / n; px[o + 1] = g / n; px[o + 2] = b / n; px[o + 3] = a / n;
  }
  return encode(size, px);
}
function writeIco(file, pngBySize) {
  const sizes = Object.keys(pngBySize).map(Number).sort((a, b) => a - b);
  const header = Buffer.alloc(6);
  header.writeUInt16LE(0, 0); header.writeUInt16LE(1, 2); header.writeUInt16LE(sizes.length, 4);
  const entries = [];
  let offset = 6 + 16 * sizes.length;
  for (const size of sizes) {
    const png = pngBySize[size];
    const entry = Buffer.alloc(16);
    entry[0] = size === 256 ? 0 : size; // 0 means 256
    entry[1] = size === 256 ? 0 : size;
    entry.writeUInt16LE(1, 4); // type: png
    entry.writeUInt16LE(1, 6); // planes
    entry.writeUInt16LE(32, 8); // bpp
    entry.writeUInt32LE(png.length, 8);
    entry.writeUInt32LE(offset, 12);
    entries.push(entry);
    offset += png.length;
  }
  fs.writeFileSync(file, Buffer.concat([header, ...entries, ...sizes.map((s) => pngBySize[s])]));
}

const out = path.resolve(__dirname, '../public');
fs.mkdirSync(out, { recursive: true });
const pngBySize = {};
for (const [file, size, margin] of [['icon-192.png', 192, 0.055], ['icon-512.png', 512, 0.055], ['icon-512-maskable.png', 512, 0.16]]) {
  const px = draw(size, margin);
  fs.writeFileSync(path.join(out, file), encode(size, px));
  pngBySize[size] = px;
  console.log('written', file);
}
// favicon-sized preview + desktop ico from the 512 render
pngBySize[32] = downsample(32, pngBySize[512], 512);
pngBySize[256] = downsample(256, pngBySize[512], 512);
// downsample returns encoded PNG bytes — write directly as a file.
fs.writeFileSync(path.join(out, 'icon-32.png'), pngBySize[32]);
console.log('written icon-32.png');
writeIco(path.join(out, 'codepluse.ico'), {
  16: downsample(16, pngBySize[512], 512),
  32: pngBySize[32],
  48: downsample(48, pngBySize[512], 512),
  64: downsample(64, pngBySize[512], 512),
  128: downsample(128, pngBySize[512], 512),
  256: pngBySize[256],
});
console.log('written codepluse.ico');
