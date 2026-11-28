import { describe, expect, it } from "vitest";
import { applyBy, heldColor, parseCss, pictureFilter, resolveColor, toCss, toHsl } from "./pptxColor";
import type { Color } from "./pptxTiming";

/** Held colours (R29) on the colours the renderer draws and the theme's. */

const palette = {
  scheme: new Map([
    ["accent2", "C0504D"],
    ["lt1", "FFFFFF"],
    ["dk1", "000000"],
  ]),
  map: new Map([
    ["bg1", "lt1"],
    ["tx1", "dk1"],
  ]),
};
const c = (type: string, attrs: Record<string, string>, mods: Color["mods"] = []): Color => ({ type, attrs, mods });
// What PowerPoint wrote for darken, lighten, desaturate and complementary colour (`emphasis`).
const DARKEN = c("hsl", { h: "0", s: "-12549", l: "-25098" });
const LIGHTEN = c("hsl", { h: "0", s: "12549", l: "25098" });
const DESATURATE = c("hsl", { h: "0", s: "-70588", l: "0" });
const COMPLEMENT = c("hsl", { h: "7200000", s: "0", l: "0" });

describe("colours", () => {
  it("reads what the renderer draws", () => {
    expect(parseCss("#3f7fce")).toEqual({ r: 63, g: 127, b: 206, a: 1 });
    expect(parseCss("#fff")).toEqual({ r: 255, g: 255, b: 255, a: 1 });
    expect(parseCss("rgb(255, 255, 255)")).toEqual({ r: 255, g: 255, b: 255, a: 1 });
    expect(parseCss("rgba(0, 0, 0, 0.35)")?.a).toBeCloseTo(0.35);
    expect(parseCss("url(#grad)")).toBe(null);
    expect(parseCss("none")).toBe(null);
  });

  it("resolves scheme colours through the theme and the colour map, with their transforms", () => {
    expect(toCss(resolveColor(c("schemeClr", { val: "accent2" }), palette)!)).toBe("rgb(192, 80, 77)");
    expect(toCss(resolveColor(c("schemeClr", { val: "tx1" }), palette)!)).toBe("rgb(0, 0, 0)");
    expect(toCss(resolveColor(c("srgbClr", { val: "4F81BD" }), palette)!)).toBe("rgb(79, 129, 189)");
    const lighter = resolveColor(c("schemeClr", { val: "accent2" }, [{ name: "lumMod", val: "60000" }, { name: "lumOff", val: "40000" }]), palette)!;
    expect(toHsl(lighter).l).toBeCloseTo(toHsl(parseCss("#C0504D")!).l * 0.6 + 0.4, 2);
    expect(resolveColor(c("schemeClr", { val: "phClr" }), palette)).toBe(null);
  });

  it("darken and lighten move lightness by a quarter; desaturate and complementary act on s and h", () => {
    const blue = parseCss("#4f81bd")!;
    const l = toHsl(blue).l;
    expect(toHsl(applyBy(blue, DARKEN)!).l).toBeCloseTo(l - 0.25098, 2);
    expect(toHsl(applyBy(blue, LIGHTEN)!).l).toBeCloseTo(l + 0.25098, 2);
    expect(toHsl(applyBy(blue, DESATURATE)!).s).toBeCloseTo(Math.max(0, toHsl(blue).s - 0.70588), 2);
    expect(toHsl(applyBy(blue, COMPLEMENT)!).h).toBeCloseTo((toHsl(blue).h + 120) % 360, 0);
  });

  it("a held part: `to` replaces the drawn colour, `by` changes apply in order on top", () => {
    const drawn = parseCss("#3f7fce")!;
    expect(toCss(heldColor(drawn, { to: c("schemeClr", { val: "accent2" }), solid: true }, palette)!)).toBe("rgb(192, 80, 77)");
    const twice = heldColor(drawn, { by: [DARKEN, DARKEN] }, palette)!;
    expect(toHsl(twice).l).toBeCloseTo(Math.max(0, toHsl(drawn).l - 0.50196), 2);
    expect(heldColor(null, { by: [DARKEN] }, palette)).toBe(null);
  });

  it("a held scheme colour drops its transforms, an sRGB one keeps them (R33, PowerPoint's film)", () => {
    const mods = (m: [string, string][]) => m.map(([name, val]) => ({ name, val }));
    for (const m of [[["tint", "40000"]], [["shade", "50000"]], [["lumMod", "60000"], ["lumOff", "40000"]], [["lumMod", "75000"]]] as [string, string][][]) {
      const to = { ...c("schemeClr", { val: "accent2" }), mods: mods(m) };
      expect(toCss(heldColor(null, { to, solid: true }, palette)!), JSON.stringify(m)).toBe("rgb(192, 80, 77)");
    }
    const green = { ...c("srgbClr", { val: "00A000" }), mods: mods([["shade", "50000"]]) };
    const g = heldColor(null, { to: green, solid: true }, palette)!;
    // The film: 0,113,0 before its +3 correction.
    expect(Math.abs(g.g - 116)).toBeLessThanOrEqual(4);
    expect(g.r + g.b).toBe(0);
  });

  it("a picture's darken stands in as a CSS filter; a `to` has none", () => {
    expect(pictureFilter({ by: [DARKEN] })).toMatch(/^brightness\(0\.49[0-9]*\) saturate/);
    expect(pictureFilter({ to: c("schemeClr", { val: "accent2" }) })).toBe(null);
  });
});

describe("colour modifiers against PowerPoint's film (fixtures/pptx-timing/colors.json, README R33)", () => {
  type Vec = { name: string; color: Parameters<typeof resolveColor>[0]; rgb: number[]; film: number[] };
  const file = (globalThis as unknown as { process: { getBuiltinModule(id: "node:fs"): { readFileSync(p: URL): Uint8Array } } }).process
    .getBuiltinModule("node:fs")
    .readFileSync(new URL("./fixtures/pptx-timing/colors.json", import.meta.url));
  const { vectors } = JSON.parse(new TextDecoder().decode(file)) as { vectors: Vec[] };
  const palette = { scheme: new Map([["accent1", "4F81BD"]]), map: new Map<string, string>() };
  it.each(vectors.map((v) => [v.name, v] as const))("%s: tint and shade in linear light, the rest in HSL", (_, v) => {
    const c = resolveColor(v.color, palette)!;
    [c.r, c.g, c.b].forEach((x, i) => {
      expect(Math.abs(x - v.rgb[i]!)).toBeLessThan(0.5);
      expect(Math.abs(x - v.film[i]!)).toBeLessThanOrEqual(4);
    });
  });
});
