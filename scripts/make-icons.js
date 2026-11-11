'use strict';
// The one icon pipeline for codepluse: renders the C+ mark at every size the
// shell and the web app need, and writes
//
//   public/codepluse.ico          16/24/32/48/64/128/256 — the desktop
//                                 shortcut, the bundle icon, and the exe's
//                                 fallback all read this one file
//   public/icon-{32,192,512}.png  PWA / favicon
//   public/icon-512-maskable.png  full-bleed, glyph inside the maskable safe zone
//   public/icon-preview/          the size ladder + wallpaper mocks, for review
//
// The two front ends are separate apps with separate icon folders, so the
// pipeline writes both:
//
//   public/                     the legacy shell UI + the .lnk / bundle icon
//     codepluse.ico             16/24/32/48/64/128/256 — desktop shortcut,
//                               bundle icon and exe fallback, one file
//     icon-{32,192,512}.png     PWA / favicon
//     icon-512-maskable.png     full-bleed, glyph inside the maskable safe zone
//   codeg-ui/public/            the Next.js workspace, which is what actually
//                               runs — it names its icons with the NxNxN suffix
//     icon-{32,128,192,512}xN.png, icon-maskable-{192,512}x512.png
//     icon.svg, icon-maskable.svg
//
// Design: "C+" — a white open arc (code brackets collapsed into a C) with a
// plus seated in its opening, on the brand indigo tile. Two shapes only, so it
// still parses at 16px.
//
// Geometry is SDF-based and supersampled, and — the part that matters for a
// .lnk — each size gets its OWN spec rather than being box-downsampled from
// 512. Below 48px the arc and plus both have to keep ~1.5px of stroke and the
// plus needs its arms clearly longer than its own thickness, or a cross turns
// into a diamond. Downsampling can't recover that.
//
// Re-run: node scripts/make-icons.js
const zlib = require('node:zlib');
const fs = require('node:fs');
const path = require('node:path');

const PUB = path.resolve(__dirname, '..', 'public');
const UI = path.resolve(__dirname, '..', 'codeg-ui', 'public');
const PREVIEW = path.join(PUB, 'icon-preview');
const ICO_SIZES = [16, 24, 32, 48, 64, 128, 256];

// ── PNG encoder (zero deps) ──
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

// ── geometry ──
function sdRoundRect(px, py, cx, cy, hw, hh, rad) {
  const qx = Math.abs(px - cx) - (hw - rad);
  const qy = Math.abs(py - cy) - (hh - rad);
  return Math.hypot(Math.max(qx, 0), Math.max(qy, 0)) + Math.min(Math.max(qx, qy), 0) - rad;
}
function sdCapsule(px, py, ax, ay, bx, by, r) {
  const pax = px - ax, pay = py - ay, bax = bx - ax, bay = by - ay;
  const t = Math.max(0, Math.min(1, (pax * bax + pay * bay) / (bax * bax + bay * bay)));
  return Math.hypot(pax - bax * t, pay - bay * t) - r;
}
// Arc of radius r / half stroke hw, centered on angle `center`, spanning +/-
// `half` around it, with round caps. Wrapping the angle delta into [-pi,pi]
// before clamping puts the opening exactly where asked at any span. Clamping
// then measuring to the centerline point is exact inside the span (reduces to
// |dist - r|) and near-exact outside it, which is what gives the ends their cap.
function sdArc(px, py, cx, cy, r, hw, center, half) {
  const dx = px - cx, dy = py - cy;
  const rad = Math.hypot(dx, dy);
  let dth = Math.atan2(dy, dx) - center;
  dth = Math.atan2(Math.sin(dth), Math.cos(dth));
  const a = Math.max(-half, Math.min(half, dth));
  return Math.hypot(rad - r, r * (dth - a)) - hw;
}
function sdPlus(px, py, cx, cy, arm, thick) {
  return Math.min(
    sdCapsule(px, py, cx, cy - arm, cx, cy + arm, thick),
    sdCapsule(px, py, cx - arm, cy, cx + arm, cy, thick)
  );
}
const clamp01 = (t) => (t < 0 ? 0 : t > 1 ? 1 : t);
const cover = (d, soft) => clamp01(0.5 - d / soft); // 1 well inside, 0 well outside
const mix = (a, b, t) => a + (b - a) * t;

const INDIGO = [[0x6d, 0x5c, 0xf0], [0x3a, 0x2f, 0xb4]];

// ── per-size spec ──
// Fractions of the canvas edge, except `weight` (arc stroke as a fraction of
// the arc radius) and the plus's arm/thickness pair, whose *ratio* is what
// makes a cross read as a cross. Three tiers, then a maskable variant.
function specFor(size, opts = {}) {
  const base = {
    size, ss: 3, pad: 0.075, radius: 0.225, glyph: 1,
    arcCx: 0.455, arcCy: 0.5, arcR: 0.276, weight: 0.31, open: 58 * Math.PI / 180,
    plusCx: 0.727, plusCy: 0.5, plusArm: 0.110, plusThick: 0.038,
    lift: true,
  };
  if (size <= 32) {
    Object.assign(base, {
      ss: 4, pad: 0.055,
      arcCx: 0.448, arcR: 0.25, weight: 0.40, open: 58 * Math.PI / 180,
      plusCx: 0.712, plusArm: 0.115, plusThick: 0.058,
      lift: false,
    });
  }
  if (size <= 24) {
    Object.assign(base, {
      ss: 5, pad: 0.05,
      arcCx: 0.44, arcR: 0.235, weight: 0.42, open: 62 * Math.PI / 180,
      plusCx: 0.70, plusArm: 0.13, plusThick: 0.075,
    });
  }
  // Maskable: the OS crops to a circle/rounded shape, so the tile must be
  // full-bleed with nothing transparent at the corners, and the glyph has to
  // stay inside the central ~80%.
  if (opts.maskable) {
    Object.assign(base, { pad: 0, radius: 0, glyph: 0.72, lift: false });
  }
  return base;
}

function paint(s) {
  const size = s.size, px = Buffer.alloc(size * size * 4);
  const tile = size * (1 - 2 * s.pad), rad = tile * s.radius;
  const tcx = size / 2, tcy = size / 2, hw = tile / 2;
  // `glyph` scales the mark about the canvas center (maskable safe zone)
  const at = (v) => tcx + (v * size - tcx) * s.glyph;
  const arcR = s.arcR * size * s.glyph, arcW = arcR * s.weight;
  const acx = at(s.arcCx), acy = at(s.arcCy);
  const pcx = at(s.plusCx), pcy = at(s.plusCy);
  const pArm = s.plusArm * size * s.glyph, pThick = s.plusThick * size * s.glyph;
  const halfSpan = Math.PI - s.open;
  // Edge ramp in device px. Must stay near 1px at small sizes — the arc stroke
  // is only ~2px there and a 2px ramp dissolves it; big sizes afford a softer edge.
  const soft = Math.min(2.6, size * 0.011 + 0.67);
  const n = s.ss * s.ss;

  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      let r = 0, g = 0, b = 0, a = 0;
      for (let sy = 0; sy < s.ss; sy++) for (let sx = 0; sx < s.ss; sx++) {
        const X = x + (sx + 0.5) / s.ss, Y = y + (sy + 0.5) / s.ss;
        const t = cover(sdRoundRect(X, Y, tcx, tcy, hw, hw, rad), soft);
        if (t <= 0) continue;

        // tile: diagonal brand gradient, plus a touch of top-light at big sizes
        let k = clamp01(((X - (tcx - hw)) + (Y - (tcy - hw))) / (2 * tile));
        if (s.lift) k = k * 0.92 + (1 - clamp01((Y - (tcy - hw)) / tile)) * 0.08;
        let tr = mix(INDIGO[0][0], INDIGO[1][0], k);
        let tg = mix(INDIGO[0][1], INDIGO[1][1], k);
        let tb = mix(INDIGO[0][2], INDIGO[1][2], k);

        const dArc = sdArc(X, Y, acx, acy, arcR, arcW / 2, Math.PI, halfSpan);
        const dPlus = sdPlus(X, Y, pcx, pcy, pArm, pThick);
        const d = Math.min(dArc, dPlus);
        const gc = cover(d, soft);
        if (gc > 0) {
          // white glyph; near the mouth the arc lifts toward cyan so light seems
          // to travel around the stroke. Too subtle to survive below 48px.
          let gr = 255, gg = 255, gb = 255;
          if (s.lift && d === dArc) {
            let th = Math.atan2(Y - acy, X - acx) - Math.PI;
            th = Math.atan2(Math.sin(th), Math.cos(th));
            const along = Math.abs(th) / halfSpan; // 0 at the back, 1 at the mouth
            const lift = clamp01((along - 0.68) / 0.32) * 0.5;
            gr = mix(255, 0xbe, lift); gg = mix(255, 0xf0, lift); gb = 255;
          } else if (s.lift) { gr = 238; gg = 250; gb = 255; }
          tr = mix(tr, gr, gc); tg = mix(tg, gg, gc); tb = mix(tb, gb, gc);
        }
        r += tr * t; g += tg * t; b += tb * t; a += t;
      }
      if (a <= 0) continue;
      const i = (y * size + x) * 4;
      // Unpremultiply by accumulated coverage, not sample count — dividing by
      // the count greys every edge pixel and leaves a halo around the tile.
      px[i] = r / a; px[i + 1] = g / a; px[i + 2] = b / a; px[i + 3] = (a / n) * 255;
    }
  }
  return px;
}

// ── ICO writer ──
// PNG-compressed entries (Vista+). Directory entry layout: 0=w 1=h 2=colors
// 3=reserved 4..5=planes 6..7=bitCount 8..11=bytesInRes 12..15=offset.
function writeIco(file, pngBySize) {
  const sizes = Object.keys(pngBySize).map(Number).sort((x, y) => x - y);
  const header = Buffer.alloc(6);
  header.writeUInt16LE(0, 0); header.writeUInt16LE(1, 2); header.writeUInt16LE(sizes.length, 4);
  const entries = [];
  let offset = 6 + 16 * sizes.length;
  for (const size of sizes) {
    const png = pngBySize[size];
    const e = Buffer.alloc(16);
    e[0] = size === 256 ? 0 : size; // 0 is how ICO encodes 256
    e[1] = size === 256 ? 0 : size;
    e.writeUInt16LE(1, 4);   // planes
    e.writeUInt16LE(32, 6);  // bitCount
    e.writeUInt32LE(png.length, 8);
    e.writeUInt32LE(offset, 12);
    entries.push(e);
    offset += png.length;
  }
  fs.writeFileSync(file, Buffer.concat([header, ...entries, ...sizes.map((z) => pngBySize[z])]));
}

fs.mkdirSync(PUB, { recursive: true });
fs.mkdirSync(PREVIEW, { recursive: true });

// the .ico: one render per size, each with its own geometry
const pngBySize = {};
for (const size of ICO_SIZES) {
  const png = encode(size, paint(specFor(size)));
  pngBySize[size] = png;
  fs.writeFileSync(path.join(PREVIEW, `icon-${size}.png`), png);
}
writeIco(path.join(PUB, 'codepluse.ico'), pngBySize);
console.log('written codepluse.ico (' + ICO_SIZES.join('/') + ')');

// PWA / favicon
for (const [file, size] of [['icon-32.png', 32], ['icon-192.png', 192], ['icon-512.png', 512]]) {
  fs.writeFileSync(path.join(PUB, file), encode(size, paint(specFor(size))));
  console.log('written', file);
}
fs.writeFileSync(path.join(PUB, 'icon-512-maskable.png'), encode(512, paint(specFor(512, { maskable: true }))));
console.log('written icon-512-maskable.png');

// ── the Next.js workspace's own set ──
// codeg-ui names its icons with the NxNxN suffix and ships vector versions, so
// the SVG is generated from the same spec numbers rather than hand-drawn —
// otherwise the browser tab and the .lnk drift apart again.
const svgFor = (opts) => {
  const s = specFor(512, opts);
  const at = (v) => 256 + (v * 512 - 256) * s.glyph;
  const R = s.arcR * 512 * s.glyph, sw = R * s.weight;
  const cx = at(s.arcCx), cy = at(s.arcCy);
  // the opening faces +x, so the stroke runs the long way from +open to -open
  const p = (a) => [cx + R * Math.cos(a), cy + R * Math.sin(a)];
  const [x0, y0] = p(s.open), [x1, y1] = p(-s.open);
  const pcx = at(s.plusCx), pcy = at(s.plusCy), pa = s.plusArm * 512 * s.glyph;
  const tile = (1 - 2 * s.pad) * 512, off = (512 - tile) / 2;
  const n = (v) => Math.round(v * 100) / 100;
  const grad = opts?.maskable
    ? `<rect width="512" height="512" fill="url(#t)"/>`
    : `<rect x="${n(off)}" y="${n(off)}" width="${n(tile)}" height="${n(tile)}" rx="${n(s.radius * tile)}" fill="url(#t)"/>`;
  // sweep=1 walks the stroke in +theta, which is the direction that passes
  // through the left side; sweep=0 bulges it through the mouth instead and
  // flips the C backwards.
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512">
<defs><linearGradient id="t" x1="0" y1="0" x2="1" y2="1">
<stop offset="0" stop-color="#${INDIGO[0].map((v) => v.toString(16).padStart(2, '0')).join('')}"/>
<stop offset="1" stop-color="#${INDIGO[1].map((v) => v.toString(16).padStart(2, '0')).join('')}"/>
</linearGradient></defs>
${grad}
<path d="M${n(x0)} ${n(y0)}A${n(R)} ${n(R)} 0 1 1 ${n(x1)} ${n(y1)}" fill="none" stroke="#fff" stroke-width="${n(sw)}" stroke-linecap="round"/>
<path d="M${n(pcx)} ${n(pcy - pa)}V${n(pcy + pa)}M${n(pcx - pa)} ${n(pcy)}H${n(pcx + pa)}" fill="none" stroke="#fff" stroke-width="${n(s.plusThick * 512 * s.glyph * 2)}" stroke-linecap="round"/>
</svg>
`;
};
fs.mkdirSync(UI, { recursive: true });
fs.writeFileSync(path.join(UI, 'icon.svg'), svgFor());
fs.writeFileSync(path.join(UI, 'icon-maskable.svg'), svgFor({ maskable: true }));
for (const size of [32, 128, 192, 512]) {
  fs.writeFileSync(path.join(UI, `icon-${size}x${size}.png`), encode(size, paint(specFor(size))));
}
for (const size of [192, 512]) {
  fs.writeFileSync(path.join(UI, `icon-maskable-${size}x${size}.png`), encode(size, paint(specFor(size, { maskable: true }))));
}
console.log('written codeg-ui/public icon.svg, icon-maskable.svg and 6 PNGs');

// ── review page: the .lnk as the shell actually draws it ──
const cell = (size, cls) =>
  `<div class="cell ${cls}"><img src="icon-${size}.png" width="${size}" height="${size}" alt=""><span>${size}</span></div>`;
const WALL_LIGHT = 'linear-gradient(135deg,#eef1fb,#c9d3ee 55%,#aebfe6)';
const WALL_DARK = 'linear-gradient(135deg,#0b1020,#1b2450 55%,#0e1a33)';
fs.writeFileSync(
  path.join(PREVIEW, 'index.html'),
  `<!DOCTYPE html><html lang="zh-CN"><head><meta charset="utf-8"><title>codepluse 图标</title><style>
body{font-family:system-ui,"Microsoft YaHei",sans-serif;background:#f4f5fa;margin:0;padding:32px;color:#1f2937}
h1{margin:0 0 6px;font-size:22px}h2{font-size:14px;margin:26px 0 10px;color:#4b5563;font-weight:600}
.lead{color:#6b7280;margin:0 0 8px;font-size:13px}
.cards{display:flex;gap:20px;flex-wrap:wrap}
.card{background:#fff;border-radius:14px;padding:16px;box-shadow:0 2px 12px rgba(0,0,0,.07)}
.row{display:flex;gap:14px;align-items:flex-end;flex-wrap:wrap;border-radius:12px;padding:16px}
.cell{display:flex;flex-direction:column;align-items:center;gap:6px}
.cell span{font-size:10px;color:#9ca3af}.row.dark span{color:#7c8bad}
.desk{display:flex;gap:26px;padding:22px;border-radius:12px}
.icon{display:flex;flex-direction:column;align-items:center;gap:6px;width:76px}
.icon img{width:44px;height:44px}
.label{font-size:12px;line-height:1.3;text-align:center;padding:2px 5px;border-radius:3px}
.desk.light .label{color:#1f2937}
.desk.dark .label{color:#fff;text-shadow:0 1px 3px rgba(0,0,0,.95)}
.bar{display:flex;align-items:center;gap:10px;height:44px;padding:0 14px;border-radius:8px;background:linear-gradient(180deg,#2a3550,#1a2338)}
.bar img{width:24px;height:24px}.bar em{font-style:normal;font-size:12px;color:#dbe3f5}
</style></head><body>
<h1>codepluse 图标</h1><p class="lead">C+ 字标：开口圆弧（代码括号收成 C）+ 加号坐在开口里，品牌靛蓝底。每个尺寸独立出图，小尺寸笔画加重而不是等比缩放。同一个 codepluse.ico 同时供桌面快捷方式与 PWA 使用。</p>
<h2>桌面（44px + 名称）</h2>
<div class="cards">
<div class="card"><div class="desk light" style="background:${WALL_LIGHT}">${'<div class="icon"><img src="icon-256.png"><div class="label">codepluse</div></div>'.repeat(3)}</div><p class="lead">浅色壁纸</p></div>
<div class="card"><div class="desk dark" style="background:${WALL_DARK}">${'<div class="icon"><img src="icon-256.png"><div class="label">codepluse</div></div>'.repeat(3)}</div><p class="lead">深色壁纸</p></div>
</div>
<h2>任务栏 / Alt-Tab（24px）</h2>
<div class="card"><div class="bar"><img src="icon-256.png"><em>codepluse · 多 Harness 工作台</em></div></div>
<h2>快捷方式与资源管理器用到的全部尺寸</h2>
<div class="card"><div class="row light" style="background:${WALL_LIGHT}">${ICO_SIZES.map((z) => cell(z, 'light')).join('')}</div></div>
<div class="card" style="margin-top:12px"><div class="row dark" style="background:${WALL_DARK}">${ICO_SIZES.map((z) => cell(z, 'dark')).join('')}</div></div>
<h2>maskable（系统会裁成圆形，图案须留在安全区内）</h2>
<div class="card"><img src="maskable.png" width="192" height="192" style="border-radius:50%"><img src="maskable.png" width="192" height="192" style="border-radius:24%"><img src="icon-512.png" width="192" height="192" style="border-radius:24%"></div>
<h2>矢量版与位图版对照（左 SVG / 右 PNG，同源数字）</h2>
<div class="card"><img src="icon.svg" width="192" height="192"><img src="icon-256.png" width="192" height="192"><img src="icon-maskable.svg" width="192" height="192" style="border-radius:50%"></div>
</body></html>`
);
fs.writeFileSync(path.join(PREVIEW, 'maskable.png'), encode(512, paint(specFor(512, { maskable: true }))));
// the SVGs live in codeg-ui/public; copy them in so this page can show them
for (const f of ['icon.svg', 'icon-maskable.svg']) fs.copyFileSync(path.join(UI, f), path.join(PREVIEW, f));
console.log('written icon-preview/index.html');
