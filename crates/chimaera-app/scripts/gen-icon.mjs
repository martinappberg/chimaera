#!/usr/bin/env node
// Generates icons/icon.png (1024x1024): the chimaera mark — a dark rounded
// square with the accent-green `>_` prompt, matching the home screen's brand
// glyph. Pure node (zlib + hand-rolled PNG chunks), no image deps; feed the
// output to `tauri icon` for the platform sets.

import { deflateSync } from "node:zlib";
import { writeFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const SIZE = 1024;
const SS = 2; // supersampling factor

// Palette (dark theme tokens from web-ui/src/app.css).
const BG = [0x17, 0x17, 0x1b];
const EDGE = [0x2b, 0x2b, 0x32];
const ACCENT = [0x3f, 0xbf, 0x85];

// macOS icon grid: 1024 canvas, ~100px transparent margin, big radius.
const BODY = { cx: 512, cy: 512, hw: 412, hh: 412, r: 186 };

// The `>` chevron: two capsule strokes meeting at the point.
const STROKE = 46;
const CHEV = [
  { ax: 330, ay: 372, bx: 476, by: 502 },
  { ax: 476, ay: 522, bx: 330, by: 652 },
];
// The `_` cursor block.
const UNDER = { x0: 540, y0: 610, x1: 712, y1: 668, r: 20 };

function sdRoundedRect(px, py, { cx, cy, hw, hh, r }) {
  const qx = Math.abs(px - cx) - (hw - r);
  const qy = Math.abs(py - cy) - (hh - r);
  const ox = Math.max(qx, 0);
  const oy = Math.max(qy, 0);
  return Math.hypot(ox, oy) + Math.min(Math.max(qx, qy), 0) - r;
}

function sdSegment(px, py, { ax, ay, bx, by }) {
  const abx = bx - ax;
  const aby = by - ay;
  const apx = px - ax;
  const apy = py - ay;
  const t = Math.max(0, Math.min(1, (apx * abx + apy * aby) / (abx * abx + aby * aby)));
  return Math.hypot(apx - t * abx, apy - t * aby);
}

const cov = (d) => Math.max(0, Math.min(1, 0.5 - d));

/** RGBA at a sample point, back-to-front composite. */
function sample(x, y) {
  let r = 0, g = 0, b = 0, a = 0;
  const put = (rgb, alpha) => {
    if (alpha <= 0) return;
    const na = alpha + a * (1 - alpha);
    r = (rgb[0] * alpha + r * a * (1 - alpha)) / na;
    g = (rgb[1] * alpha + g * a * (1 - alpha)) / na;
    b = (rgb[2] * alpha + b * a * (1 - alpha)) / na;
    a = na;
  };

  const body = sdRoundedRect(x, y, BODY);
  put(BG, cov(body));
  // Hairline inner edge, one token lighter, for definition on dark docks.
  put(EDGE, cov(Math.abs(body + 3) - 3) * 0.55);

  const chev = Math.min(
    sdSegment(x, y, CHEV[0]) - STROKE / 2,
    sdSegment(x, y, CHEV[1]) - STROKE / 2,
  );
  const under = sdRoundedRect(x, y, {
    cx: (UNDER.x0 + UNDER.x1) / 2,
    cy: (UNDER.y0 + UNDER.y1) / 2,
    hw: (UNDER.x1 - UNDER.x0) / 2,
    hh: (UNDER.y1 - UNDER.y0) / 2,
    r: UNDER.r,
  });
  // Clip the glyph to the body so anti-aliased edges never bleed outside.
  put(ACCENT, Math.min(cov(Math.min(chev, under)), cov(body)));
  return [r, g, b, a];
}

function render() {
  const px = Buffer.alloc(SIZE * SIZE * 4);
  for (let y = 0; y < SIZE; y++) {
    for (let x = 0; x < SIZE; x++) {
      let r = 0, g = 0, b = 0, a = 0;
      for (let sy = 0; sy < SS; sy++) {
        for (let sx = 0; sx < SS; sx++) {
          const [sr, sg, sb, sa] = sample(x + (sx + 0.5) / SS, y + (sy + 0.5) / SS);
          r += sr * sa;
          g += sg * sa;
          b += sb * sa;
          a += sa;
        }
      }
      const n = SS * SS;
      const i = (y * SIZE + x) * 4;
      px[i] = a > 0 ? Math.round(r / a) : 0;
      px[i + 1] = a > 0 ? Math.round(g / a) : 0;
      px[i + 2] = a > 0 ? Math.round(b / a) : 0;
      px[i + 3] = Math.round((a / n) * 255);
    }
  }
  return px;
}

// --- minimal PNG writer ------------------------------------------------------

const CRC_TABLE = new Uint32Array(256).map((_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});

function crc32(buf) {
  let c = 0xffffffff;
  for (const byte of buf) c = CRC_TABLE[(c ^ byte) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const out = Buffer.alloc(12 + data.length);
  out.writeUInt32BE(data.length, 0);
  out.write(type, 4, "ascii");
  data.copy(out, 8);
  out.writeUInt32BE(crc32(out.subarray(4, 8 + data.length)), 8 + data.length);
  return out;
}

function png(pixels) {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(SIZE, 0);
  ihdr.writeUInt32BE(SIZE, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 6; // RGBA
  const raw = Buffer.alloc(SIZE * (SIZE * 4 + 1));
  for (let y = 0; y < SIZE; y++) {
    raw[y * (SIZE * 4 + 1)] = 0; // filter: none
    pixels.copy(raw, y * (SIZE * 4 + 1) + 1, y * SIZE * 4, (y + 1) * SIZE * 4);
  }
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(raw, { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

const here = dirname(fileURLToPath(import.meta.url));
const out = join(here, "..", "icons", "icon.png");
mkdirSync(dirname(out), { recursive: true });
writeFileSync(out, png(render()));
console.log(`wrote ${out}`);
