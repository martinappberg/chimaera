#!/usr/bin/env node
// Generates icons/icon.png (1024x1024): the chimaera hexagon mark on a light
// tile, matching the home-screen brand mark (web-ui/src/lib/BrandMark.svelte)
// and the favicon (web-ui/public/favicon.svg) — a charcoal→silver metallic
// stroke drawing a hexagon shell around a "C" monogram. Pure node (zlib +
// hand-rolled PNG chunks), no image deps; feed the output to `tauri icon`
// for the platform sets.

import { deflateSync } from "node:zlib";
import { writeFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const SIZE = 1024;
const SS = 3; // supersampling factor (thin metallic strokes want the extra AA)

// Light tile + the metallic gradient stops (charcoal → mid → silver).
const TILE = [0xf5, 0xf5, 0xf6];
const DARK = [0x2c, 0x2c, 0x30];
const MID = [0x6f, 0x6f, 0x74];
const LIGHT = [0xee, 0xee, 0xf1];

// macOS icon grid: 1024 canvas, ~100px transparent margin, big radius.
const BODY = { cx: 512, cy: 512, hw: 412, hh: 412, r: 186 };

// Mark geometry, mapped from the 240-unit BrandMark grid: icon = 100 + p·3.433.
const STROKE = 52;
const HEXV = [
  [512, 258],
  [739, 388],
  [739, 636],
  [512, 766],
  [285, 636],
  [285, 388],
];

// The inner "C": a 264° arc (open on the right) sampled into short segments,
// so the capsule union renders it as one smooth round stroke.
const C_CX = 512;
const C_CY = 512;
const C_R = 161; // 47 · 3.433
function arcSegments(cx, cy, r, degFrom, degTo, n) {
  const pt = (deg) => {
    const a = (deg * Math.PI) / 180;
    return [cx + r * Math.cos(a), cy + r * Math.sin(a)];
  };
  const segs = [];
  for (let i = 0; i < n; i++) {
    const [ax, ay] = pt(degFrom + ((degTo - degFrom) * i) / n);
    const [bx, by] = pt(degFrom + ((degTo - degFrom) * (i + 1)) / n);
    segs.push({ ax, ay, bx, by });
  }
  return segs;
}

// Every stroke as a segment; round caps/joins fall out of the capsule union.
const SEGMENTS = [
  ...HEXV.map((a, i) => ({ ax: a[0], ay: a[1], bx: HEXV[(i + 1) % 6][0], by: HEXV[(i + 1) % 6][1] })),
  ...arcSegments(C_CX, C_CY, C_R, 48, 312, 40),
];
// The gradient runs left→right across the hexagon's horizontal span.
const GRAD_X0 = 285;
const GRAD_X1 = 739;

function lerp(a, b, t) {
  return [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t];
}

/** Metallic colour at horizontal position x (charcoal → mid → silver). */
function gradAt(x) {
  const t = Math.max(0, Math.min(1, (x - GRAD_X0) / (GRAD_X1 - GRAD_X0)));
  return t < 0.5 ? lerp(DARK, MID, t * 2) : lerp(MID, LIGHT, (t - 0.5) * 2);
}

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
  put(TILE, cov(body));

  // Union of every stroke (capsule = segment distance − half width).
  let mark = Infinity;
  for (const s of SEGMENTS) mark = Math.min(mark, sdSegment(x, y, s) - STROKE / 2);
  // Clip the mark to the tile so anti-aliased edges never bleed outside.
  put(gradAt(x), Math.min(cov(mark), cov(body)));
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
