// Icon design candidates for codepluse — generates preview PNGs for each
// variant at multiple sizes plus a comparison page. Run:
//   node scripts/icon-designs.mjs
// Picks up the SDF/AA machinery style from make-icons.js; each variant is a
// (tile, glyph) painter pair evaluated per supersample.
import zlib from "node:zlib";
import fs from "node:fs";
import path from "node:path";

const OUT = path.resolve(process.cwd(), "public", "icon-preview");
const SSR = 3; // supersample root

// ── PNG encode ──
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
  const t = Buffer.from(type, "ascii");
  return Buffer.concat([len, t, data, Buffer.concat([Buffer.alloc(4), t]).subarray(0, 0), crc32b(t, data)]);
}
function crc32b(t, data) {
  const crc = Buffer.alloc(4); crc.writeUInt32BE(crc32(Buffer.concat([t, data])));
  return crc;
}
function encode(size, px) {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0); ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8; ihdr[9] = 6;
  const raw = Buffer.alloc(size * (size * 4 + 1));
  for (let y = 0; y < size; y++) {
    raw[y * (size * 4 + 1)] = 0;
    px.copy(raw, y * (size * 4 + 1) + 1, y * size * 4, (y + 1) * size * 4);
  }
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr), chunk("IDAT", zlib.deflateSync(raw, { level: 9 })), chunk("IEND", Buffer.alloc(0)),
  ]);
}

// ── SDF helpers ──
const sdRoundRect = (px, py, cx, cy, hw, hh, rad) => {
  const qx = Math.abs(px - cx) - (hw - rad);
  const qy = Math.abs(py - cy) - (hh - rad);
  return Math.hypot(Math.max(qx, 0), Math.max(qy, 0)) + Math.min(Math.max(qx, qy), 0) - rad;
};
const sdCircle = (px, py, cx, cy, r) => Math.hypot(px - cx, py - cy) - r;
const sdCapsule = (px, py, ax, ay, bx, by, r) => {
  const pax = px - ax, pay = py - ay, bax = bx - ax, bay = by - ay;
  const t = Math.max(0, Math.min(1, (pax * bax + pay * bay) / (bax * bax + bay * bay)));
  return Math.hypot(pax - bax * t, pay - bay * t) - r;
};
const smooth = (d, soft) => Math.max(0, Math.min(1, 0.5 - d / soft));
const mix = (a, b, t) => a + (b - a) * t;
const mix3 = (c1, c2, c3, t) => (t < 0.5 ? [mix(c1[0], c2[0], t * 2), mix(c1[1], c2[1], t * 2), mix(c1[2], c2[2], t * 2)] : [mix(c2[0], c3[0], (t - 0.5) * 2), mix(c2[1], c3[1], (t - 0.5) * 2), mix(c2[2], c3[2], (t - 0.5) * 2)]);

// ── variant definitions ──
// Each: { tile(u,v)->[r,g,b], glyph(px,py)->coverage 0..1 } evaluated in a
// normalized space where the tile spans [0,1]² (callers scale).
const VARIANTS = [
  {
    id: "converge",
    label: "A · 汇聚（浅底渐变流线）",
    tile: (u, v) => {
      const t = Math.min(1, Math.max(0, (u + v) / 2));
      return [mix(0xe8, 0xdde, t), mix(0xec, 0xe4, t), mix(0xff, 0xfc, t)];
    },
    glyph(px, py) {
      const c = 0.5, nodes = [[0.20, 0.16], [0.84, 0.26], [0.5, 0.87]];
      const cols = [[34, 211, 238], [139, 92, 246], [236, 72, 153]];
      let cov = 0;
      nodes.forEach(([nx, ny], i) => {
        cov = Math.max(cov, smooth(sdCapsule(px, py, nx, ny, c, c, 0.035), 0.012) * 0.9);
      });
      const hub = smooth(sdCircle(px, py, c, c, 0.105), 0.012);
      const hubInner = smooth(sdCircle(px, py, c, c, 0.062), 0.012);
      cov = Math.max(cov, hub);
      nodes.forEach(([nx, ny], i) => {
        cov = Math.max(cov, smooth(sdCircle(px, py, nx, ny, 0.075), 0.012));
      });
      // store nearest-node index for coloring via closure side channel
      let best = 0, bestD = 9;
      nodes.forEach(([nx, ny], i) => { const d = Math.hypot(px - nx, py - ny); if (d < bestD) { bestD = d; best = i; } });
      const centerish = Math.hypot(px - c, py - c);
      const col = centerish < 0.105 ? [255, 255, 255]
        : centerish < 0.19 ? mix3(cols[0], cols[1], cols[2], 0.5)
        : cols[best];
      return { cov, col };
    },
  },
  {
    id: "spark",
    label: "B · 星枢（深底霓虹）",
    tile: (u, v) => {
      const d = Math.hypot(u - 0.5, v - 0.5);
      const t = Math.min(1, d * 1.4);
      return [mix(0x0b, 0x0d, t), mix(0x10, 0x12, t), mix(0x20, 0x26, t)];
    },
    glyph(px, py) {
      const c = 0.5;
      const nodes = [[0.5, 0.14], [0.14, 0.78], [0.86, 0.78]];
      let cov = 0;
      nodes.forEach(([nx, ny]) => {
        cov = Math.max(cov, smooth(sdCapsule(px, py, c, c, nx, ny, 0.018), 0.012) * 0.85);
        cov = Math.max(cov, smooth(sdCircle(px, py, nx, ny, 0.052), 0.012));
      });
      // center node with glow
      const d = Math.hypot(px - c, py - c);
      const glow = Math.max(0, 1 - d / 0.22) * 0.35;
      cov = Math.max(cov, smooth(sdCircle(px, py, c, c, 0.115), 0.02));
      cov = Math.min(1, cov + glow * 0.6);
      const ang = Math.atan2(py - c, px - c) / (Math.PI * 2) + 0.5;
      const col = mix3([34, 211, 238], [139, 92, 246], [236, 72, 153], ang);
      const white = smooth(sdCircle(px, py, c, c, 0.06), 0.015);
      return { cov, col: white ? [255, 255, 255] : col };
    },
  },
  {
    id: "cplus",
    label: "C · C+ 徽章（品牌渐变）",
    tile: (u, v) => {
      const t = Math.min(1, Math.max(0, (u + v) / 2));
      return [mix(0x6d, 0x4a, t), mix(0x5c, 0x5c, t), mix(0xf0, 0xe0, t)];
    },
    glyph(px, py) {
      const c = 0.5;
      const dOuter = sdCircle(px, py, c, c, 0.30);
      const dInner = sdCircle(px, py, c, c, 0.155);
      const ang = ((Math.atan2(py - c, px - c) / Math.PI) * 180 + 360) % 360;
      // C arc: gap toward the right (from -52° to 52° removed)
      const inGap = ang < 52 || ang > 308;
      let cov = inGap ? 0 : smooth(Math.max(dOuter, -dInner), 0.014);
      // plus in the opening
      const plus = Math.min(
        sdCapsule(px, py, 0.5 + 0.16, c, 0.5 + 0.16, c, 0),
        0,
      );
      const px2 = sdCapsule(px, py, 0.66, c - 0.075, 0.66, c + 0.075, 0.030);
      const px3 = sdCapsule(px, py, 0.66 - 0.075, c, 0.66 + 0.075, c, 0.030);
      cov = Math.max(cov, smooth(Math.min(px2, px3), 0.014));
      const light = Math.min(1, Math.max(0, (py - 0.2) / 0.6));
      const col = [mix(0xc7, 0xff, light), mix(0xd2, 0xff, light), mix(0xfe, 0xff, light)];
      return { cov, col };
    },
  },
  {
    id: "hub",
    label: "D · 现行枢纽环（对照）",
    tile: (u, v) => {
      const t = Math.min(1, Math.max(0, (u + v) / 2));
      return [mix(0x6d, 0x4a, t), 0x5c, mix(0xf0, 0xe0, t)];
    },
    glyph(px, py) {
      const c = 0.5;
      const hubR = 0.135, ring = 0.038;
      let d = sdCircle(px, py, c, c, hubR);
      d = Math.max(d, -sdCircle(px, py, c, c, hubR - ring));
      const spokeLen = 0.30, nodeR = 0.058, spokeW = 0.030;
      for (const a of [-90, 30, 150].map((x) => (x * Math.PI) / 180)) {
        const nx = c + Math.cos(a) * spokeLen, ny = c + Math.sin(a) * spokeLen;
        d = Math.min(d, sdCapsule(px, py, c + Math.cos(a) * hubR * 0.9, c + Math.sin(a) * hubR * 0.9, nx, ny, spokeW));
        d = Math.min(d, sdCircle(px, py, nx, ny, nodeR));
      }
      const cov = smooth(d, 0.012);
      const plus = Math.min(
        sdCapsule(px, py, c, c - 0.075, c, c + 0.075, 0.028),
        sdCapsule(px, py, c - 0.075, c, c + 0.075, c, 0.028),
      );
      const cov2 = Math.max(cov, smooth(plus, 0.012));
      return { cov: cov2, col: [255, 255, 255] };
    },
  },
];

// ── render ──
function render(variant, size, margin = 0.06) {
  const px = Buffer.alloc(size * size * 4);
  const SS = 3;
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      let r = 0, g = 0, b = 0, a = 0;
      for (let sy = 0; sy < SS; sy++) for (let sx = 0; sx < SS; sx++) {
        const sxp = (x + (sx + 0.5) / SS) / size;
        const syp = (y + (sy + 0.5) / SS) / size;
        const [tr, tg, tb] = variant.tile(sxp, syp);
        const { cov, col } = variant.glyph(sxp, syp);
        const cr = tr * (1 - cov) + col[0] * cov;
        const cg = tg * (1 - cov) + col[1] * cov;
        const cb = tb * (1 - cov) + col[2] * cov;
        // rounded-rect mask
        const d = sdRoundRect(sxp, syp, 0.5, 0.5, 0.5 - margin, 0.5 - margin, 0.225 - margin * 0.5);
        const ma = smooth(d, 1.6 / size);
        r += cr * ma; g += cg * ma; b += cb * ma; a += ma;
      }
      const n = SS * SS;
      const i = (y * size + x) * 4;
      px[i] = r / n; px[i + 1] = g / n; px[i + 2] = b / n; px[i + 3] = (a / n) * 255;
    }
  }
  return encode(size, px);
}

fs.mkdirSync(OUT, { recursive: true });
for (const variant of VARIANTS) {
  for (const size of [512, 128, 64, 32, 16]) {
    fs.writeFileSync(path.join(OUT, `${variant.id}-${size}.png`), render(variant, size));
  }
  console.log("rendered", variant.id);
}

const cells = VARIANTS.map((v) => `
<div class="variant">
  <h2>${v.label}</h2>
  <div class="row light"><img src="${v.id}-512.png" width="160"><img src="${v.id}-128.png" width="80"><img src="${v.id}-64.png" width="40"><img src="${v.id}-32.png" width="20"><img src="${v.id}-16.png" width="16"></div>
  <div class="row dark"><img src="${v.id}-512.png" width="96"><img src="${v.id}-64.png" width="40"><img src="${v.id}-16.png" width="16"></div>
  <p class="cap">浅色页 / 深色任务栏 · 512/128/64/32/16</p>
</div>`).join("\n");

fs.writeFileSync(
  path.join(OUT, "index.html"),
  `<!DOCTYPE html><html lang="zh-CN"><head><meta charset="utf-8"><title>codepluse 图标候选</title><style>
body{font-family:system-ui,sans-serif;background:#f4f5fa;margin:0;padding:32px;color:#1f2937}
h1{margin:0 0 8px}p.lead{color:#6b7280;margin:0 0 24px}
.grid{display:grid;grid-template-columns:1fr 1fr;gap:24px}
.variant{background:#fff;border-radius:16px;padding:20px;box-shadow:0 2px 12px rgba(0,0,0,.06)}
h2{font-size:15px;margin:0 0 14px}
.row{display:flex;gap:18px;align-items:center}
.row.light{background:#f8f9ff;padding:16px;border-radius:12px;margin-bottom:10px}
.row.dark{background:#0b1020;padding:16px;border-radius:12px}
.cap{font-size:11px;color:#9ca3af;margin:10px 0 0}
</style></head><body><h1>codepluse 图标候选方案</h1><p class="lead">挑一个（或说组合意见），我把它接进正式管线并生成全套尺寸。</p><div class="grid">${cells}</div></body></html>`,
);
console.log("preview written:", path.join(OUT, "index.html"));
