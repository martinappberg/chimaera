/**
 * Colour arithmetic for held emphasis colours (the timing contract's R29):
 * a colour PowerPoint wrote (`schemeClr`, `srgbClr`, … with its child
 * transforms) resolved against the slide's theme, an `hsl`/`rgb` offset
 * (`by`) applied to a drawn colour, and the CSS colours the renderer
 * draws, read and written. Pure, no DOM.
 */

import type { Color, HeldColor } from "./pptxTiming";

export interface Rgba {
  r: number;
  g: number;
  b: number;
  /** 0–1 */
  a: number;
}

/** A slide's theme colours: scheme name (`accent2`, `dk1`, …) → `#rrggbb`,
 *  and the colour map (`bg1` → `lt1`, …). */
export interface Palette {
  scheme: ReadonlyMap<string, string>;
  map: ReadonlyMap<string, string>;
}

const clamp01 = (n: number) => Math.min(1, Math.max(0, n));
const byte = (n: number) => Math.round(Math.min(255, Math.max(0, n)));

/** `#rgb`, `#rrggbb`, `#rrggbbaa`, `rgb()`/`rgba()`; null otherwise. */
export function parseCss(css: string): Rgba | null {
  const s = css.trim().toLowerCase();
  let m = /^#([0-9a-f]{3})$/.exec(s);
  if (m !== null) {
    const [r, g, b] = [...m[1]!].map((c) => parseInt(c + c, 16));
    return { r: r!, g: g!, b: b!, a: 1 };
  }
  m = /^#([0-9a-f]{6})([0-9a-f]{2})?$/.exec(s);
  if (m !== null) {
    const n = parseInt(m[1]!, 16);
    return { r: (n >> 16) & 255, g: (n >> 8) & 255, b: n & 255, a: m[2] === undefined ? 1 : parseInt(m[2], 16) / 255 };
  }
  m = /^rgba?\(\s*([0-9.]+)[\s,]+([0-9.]+)[\s,]+([0-9.]+)(?:\s*[,/]\s*([0-9.]+)(%?))?\s*\)$/.exec(s);
  if (m !== null) {
    const a = m[4] === undefined ? 1 : m[5] === "%" ? Number(m[4]) / 100 : Number(m[4]);
    const c = { r: Number(m[1]), g: Number(m[2]), b: Number(m[3]), a: clamp01(a) };
    return [c.r, c.g, c.b].every(Number.isFinite) ? { ...c, r: byte(c.r), g: byte(c.g), b: byte(c.b) } : null;
  }
  return null;
}

export function toCss(c: Rgba): string {
  return c.a >= 1 ? `rgb(${byte(c.r)}, ${byte(c.g)}, ${byte(c.b)})` : `rgba(${byte(c.r)}, ${byte(c.g)}, ${byte(c.b)}, ${Math.round(c.a * 1000) / 1000})`;
}

export function mix(a: Rgba, b: Rgba, t: number): Rgba {
  const k = clamp01(t);
  return { r: a.r + (b.r - a.r) * k, g: a.g + (b.g - a.g) * k, b: a.b + (b.b - a.b) * k, a: a.a + (b.a - a.a) * k };
}

/** RGB (0–255) → HSL (h in degrees, s and l 0–1). */
export function toHsl(c: Rgba): { h: number; s: number; l: number } {
  const r = c.r / 255;
  const g = c.g / 255;
  const b = c.b / 255;
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const l = (max + min) / 2;
  if (max === min) return { h: 0, s: 0, l };
  const d = max - min;
  const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
  const h = max === r ? (g - b) / d + (g < b ? 6 : 0) : max === g ? (b - r) / d + 2 : (r - g) / d + 4;
  return { h: h * 60, s, l };
}

export function fromHsl(h: number, s: number, l: number, a = 1): Rgba {
  const hh = (((h % 360) + 360) % 360) / 360;
  const ss = clamp01(s);
  const ll = clamp01(l);
  if (ss === 0) return { r: ll * 255, g: ll * 255, b: ll * 255, a };
  const q = ll < 0.5 ? ll * (1 + ss) : ll + ss - ll * ss;
  const p = 2 * ll - q;
  const f = (t: number) => {
    const u = t < 0 ? t + 1 : t > 1 ? t - 1 : t;
    if (u < 1 / 6) return p + (q - p) * 6 * u;
    if (u < 1 / 2) return q;
    if (u < 2 / 3) return p + (q - p) * (2 / 3 - u) * 6;
    return p;
  };
  return { r: f(hh + 1 / 3) * 255, g: f(hh) * 255, b: f(hh - 1 / 3) * 255, a };
}

const num = (v: string | undefined | null): number | null => {
  if (v === undefined || v === null || !/^-?[0-9]{1,10}$/.test(v)) return null;
  return Number(v);
};

const SYS = new Map([
  ["window", "#ffffff"],
  ["windowText", "#000000"],
]);
const PRESET = new Map([
  ["black", "#000000"],
  ["white", "#ffffff"],
  ["red", "#ff0000"],
  ["green", "#008000"],
  ["blue", "#0000ff"],
  ["yellow", "#ffff00"],
]);
/** The scheme names the colour map routes (`bg1` → `lt1`, …). */
const MAPPED = new Set(["bg1", "bg2", "tx1", "tx2"]);

/**
 * The colour `c` names, with its child transforms (ECMA-376 20.1.2.3: the
 * luminance, saturation and hue ones in HSL, tint and shade towards white
 * and black, alpha), or null when it names something this can't resolve
 * (`phClr`, an unknown preset or system colour).
 */
const toLinear = (v: number) => {
  const x = clamp01(v / 255);
  return x <= 0.04045 ? x / 12.92 : ((x + 0.055) / 1.055) ** 2.4;
};
const toSrgb = (x: number) => (clamp01(x) <= 0.0031308 ? 12.92 * clamp01(x) : 1.055 * clamp01(x) ** (1 / 2.4) - 0.055) * 255;

/** A change made to each channel in linear light. */
function linearly(c: Rgba, f: (x: number) => number): Rgba {
  return { r: toSrgb(f(toLinear(c.r))), g: toSrgb(f(toLinear(c.g))), b: toSrgb(f(toLinear(c.b))), a: c.a };
}

export function resolveColor(c: Color, palette: Palette): Rgba | null {
  const at = (k: string) => c.attrs[k];
  let base: Rgba | null = null;
  switch (c.type) {
    case "srgbClr":
      base = parseCss(`#${at("val") ?? ""}`);
      break;
    case "schemeClr": {
      const name = at("val") ?? "";
      const mapped = MAPPED.has(name) ? palette.map.get(name) : name;
      const hex = mapped === undefined ? undefined : palette.scheme.get(mapped);
      base = hex === undefined ? null : parseCss(hex.startsWith("#") ? hex : `#${hex}`);
      break;
    }
    case "sysClr": {
      const last = at("lastClr");
      base = parseCss(last !== undefined ? `#${last}` : (SYS.get(at("val") ?? "") ?? ""));
      break;
    }
    case "prstClr":
      base = parseCss(PRESET.get(at("val") ?? "") ?? "");
      break;
    case "hslClr": {
      const h = num(at("hue"));
      const s = num(at("sat"));
      const l = num(at("lum"));
      base = h === null || s === null || l === null ? null : fromHsl(h / 60000, s / 100000, l / 100000);
      break;
    }
    case "scrgbClr": {
      const lin = (v: string | undefined) => {
        const n = num(v);
        if (n === null) return null;
        const x = clamp01(n / 100000);
        return (x <= 0.0031308 ? 12.92 * x : 1.055 * x ** (1 / 2.4) - 0.055) * 255;
      };
      const [r, g, b] = [lin(at("r")), lin(at("g")), lin(at("b"))];
      base = r === null || g === null || b === null ? null : { r, g, b, a: 1 };
      break;
    }
  }
  if (base === null) return null;
  let out = base;
  for (const m of c.mods) {
    const v = num(m.val);
    if (v === null) continue;
    const f = v / 100000;
    const hsl = toHsl(out);
    switch (m.name) {
      case "lumMod":
        out = fromHsl(hsl.h, hsl.s, hsl.l * f, out.a);
        break;
      case "lumOff":
        out = fromHsl(hsl.h, hsl.s, hsl.l + f, out.a);
        break;
      case "satMod":
        out = fromHsl(hsl.h, hsl.s * f, hsl.l, out.a);
        break;
      case "satOff":
        out = fromHsl(hsl.h, hsl.s + f, hsl.l, out.a);
        break;
      case "hueMod":
        out = fromHsl(hsl.h * f, hsl.s, hsl.l, out.a);
        break;
      case "hueOff":
        out = fromHsl(hsl.h + v / 60000, hsl.s, hsl.l, out.a);
        break;
      // Tint and shade work in linear light (PowerPoint's film of flat
      // accent fills: a 50 % shade of 4F81BD is 57,94,137, not 40,65,95);
      // the other modifiers in sRGB's HSL, as the same film shows.
      case "tint":
        out = linearly(out, (x) => 1 - clamp01(f) * (1 - x));
        break;
      case "shade":
        out = linearly(out, (x) => clamp01(f) * x);
        break;
      case "alpha":
        out = { ...out, a: clamp01(f) };
        break;
      case "alphaMod":
        out = { ...out, a: clamp01(out.a * f) };
        break;
      case "alphaOff":
        out = { ...out, a: clamp01(out.a + f) };
        break;
    }
  }
  return out;
}

/** A `by` change on a colour: an `hsl` offset (`h` in 60000ths of a degree,
 *  `s`, `l` in 100000ths) or an `rgb` one (`r` `g` `b` in 100000ths of
 *  full scale); null when `by` is neither. */
export function applyBy(c: Rgba, by: Color): Rgba | null {
  if (by.type === "hsl") {
    const [h, s, l] = [num(by.attrs.h) ?? 0, num(by.attrs.s) ?? 0, num(by.attrs.l) ?? 0];
    const x = toHsl(c);
    return fromHsl(x.h + h / 60000, x.s + s / 100000, x.l + l / 100000, c.a);
  }
  if (by.type === "rgb") {
    const k = (v: string | undefined) => ((num(v) ?? 0) / 100000) * 255;
    return { r: byte(c.r + k(by.attrs.r)), g: byte(c.g + k(by.attrs.g)), b: byte(c.b + k(by.attrs.b)), a: c.a };
  }
  return null;
}

/** The colour a held part shows over a drawn colour `drawn` (null: no
 *  drawn colour, a `to` then stands alone), or null when it can't be
 *  resolved. */
export function heldColor(drawn: Rgba | null, held: HeldColor, palette: Palette): Rgba | null {
  // A held scheme colour is the scheme colour alone, on a fill, a line and
  // text: PowerPoint's film of the corpus `colours` deck holds plain accent2
  // for accent2 with a tint, a shade or lumMod/lumOff, while an sRGB colour's
  // shade applies (README R33).
  const to = held.to?.type === "schemeClr" ? { ...held.to, mods: [] } : held.to;
  let c = to === undefined ? drawn : resolveColor(to, palette);
  if (c === null) return null;
  for (const by of held.by ?? []) {
    const next = applyBy(c, by);
    if (next === null) return null;
    c = next;
  }
  return c;
}

/** CSS filters standing in for `by` changes on a picture, which has no
 *  single colour to change (hsl offsets only): null when there is none. */
export function pictureFilter(held: HeldColor): string | null {
  if (held.to !== undefined || held.by === undefined || held.by.length === 0) return null;
  let h = 0;
  let s = 0;
  let l = 0;
  for (const by of held.by) {
    if (by.type !== "hsl") return null;
    h += (num(by.attrs.h) ?? 0) / 60000;
    s += (num(by.attrs.s) ?? 0) / 100000;
    l += (num(by.attrs.l) ?? 0) / 100000;
  }
  const parts: string[] = [];
  if (l !== 0) parts.push(`brightness(${Math.max(0, 1 + 2 * l).toFixed(3)})`);
  if (s !== 0) parts.push(`saturate(${Math.max(0, 1 + s * 2).toFixed(3)})`);
  if (h !== 0) parts.push(`hue-rotate(${h.toFixed(1)}deg)`);
  return parts.length === 0 ? null : parts.join(" ");
}
