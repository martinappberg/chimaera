import { describe, expect, it } from "vitest";
import { THEMES } from "./themes";

// The legibility contract themes.ts states, enforced: every color a theme
// uses AS TEXT holds its floor against the sheet it is read on. Lift a
// failing tone by OKLab lightness only (hue and chroma untouched) so the
// theme still reads as itself.

const hex = (h: string) => [0, 2, 4].map((i) => parseInt(h.slice(1 + i, 3 + i), 16) / 255);
const lin = (c: number) => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
const luminance = (h: string) => {
  const [r, g, b] = hex(h).map(lin);
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
};
const ratio = (a: string, b: string) => {
  const x = luminance(a);
  const y = luminance(b);
  return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
};
// sRGB color-mix(in srgb, a p%, b), as the browser computes it.
const mix = (a: string, b: string, p: number) => {
  const A = hex(a);
  const B = hex(b);
  return "#" + A.map((v, i) => Math.round((v * p + B[i] * (1 - p)) * 255).toString(16).padStart(2, "0")).join("");
};

const TEXT_TOKENS = [
  "--fg",
  "--muted",
  "--err",
  "--warn", // renders as note text, not only a dot
  "--syn-keyword",
  "--syn-string",
  "--syn-number",
  "--syn-type",
  "--syn-func",
  "--syn-def",
  "--syn-prop",
] as const;

const ANSI_NORMAL = ["red", "green", "yellow", "blue", "magenta", "cyan"] as const;
const ANSI_BRIGHT = [
  "brightBlack",
  "brightRed",
  "brightGreen",
  "brightYellow",
  "brightBlue",
  "brightMagenta",
  "brightCyan",
] as const;

describe("theme text contrast", () => {
  for (const theme of THEMES) {
    const sheet = theme.tokens["--term-bg"];
    describe(theme.id, () => {
      it("--accent as text (app.css --accent-text: 75% toward --fg) holds 4.5:1", () => {
        expect(ratio(mix(theme.tokens["--accent"], theme.tokens["--fg"], 0.75), sheet)).toBeGreaterThanOrEqual(4.5);
      });
      it("--rate (a glyph tone, never prose) holds 3:1", () => {
        expect(ratio(theme.tokens["--rate"], sheet)).toBeGreaterThanOrEqual(3.0);
      });
      for (const token of TEXT_TOKENS) {
        it(`${token} holds 4.5:1 on the sheet`, () => {
          expect(ratio(theme.tokens[token], sheet)).toBeGreaterThanOrEqual(4.5);
        });
      }
      it("--syn-comment holds 3.8:1 (dimmer than code, still readable)", () => {
        expect(ratio(theme.tokens["--syn-comment"], sheet)).toBeGreaterThanOrEqual(3.8);
      });
      it("--muted holds on the workspace surfaces it is read on", () => {
        expect(ratio(theme.tokens["--muted"], theme.tokens["--bg"])).toBeGreaterThanOrEqual(4.5);
        expect(ratio(theme.tokens["--muted"], theme.tokens["--rail-bg"])).toBeGreaterThanOrEqual(4.5);
        expect(ratio(theme.tokens["--muted"], theme.tokens["--row-active"])).toBeGreaterThanOrEqual(4.0);
      });
      for (const name of ANSI_NORMAL) {
        it(`ansi ${name} holds 4.5:1 on the terminal`, () => {
          expect(ratio(theme.ansi[name], sheet)).toBeGreaterThanOrEqual(4.5);
        });
      }
      for (const name of ANSI_BRIGHT) {
        it(`ansi ${name} holds 3.5:1 on the terminal`, () => {
          expect(ratio(theme.ansi[name], sheet)).toBeGreaterThanOrEqual(3.5);
        });
      }
    });
  }
});
