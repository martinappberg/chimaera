import { describe, expect, it } from "vitest";
import { encodePageTheme, PAGE_THEME_TOKENS } from "./pageTheme";
import { pageMessageText } from "./pageMessage";

/** What the page API does with the URL value (`chimaera-core` `page_api.js`). */
function decodeLikeThePage(raw: string): unknown {
  const b64 = raw.replace(/-/g, "+").replace(/_/g, "/");
  return JSON.parse(new TextDecoder().decode(Uint8Array.from(atob(b64), (c) => c.charCodeAt(0))));
}

describe("encodePageTheme", () => {
  it("round-trips through the page's own decoding, non-ASCII font names too", () => {
    const theme = { m: "dark" as const, v: { bg: "#111", font: '"Inter", system-ui', accent: "color-mix(in srgb, #2e9e6b 75%, #eee)", mono: "“Söhne Mono”" } };
    const raw = encodePageTheme(theme);
    expect(raw).toMatch(/^[A-Za-z0-9_-]+$/);
    expect(decodeLikeThePage(raw)).toEqual(theme);
  });
});

describe("PAGE_THEME_TOKENS", () => {
  it("names each page variable once, in the page API's name grammar", () => {
    const names = PAGE_THEME_TOKENS.map(([name]) => name);
    expect(new Set(names).size).toBe(names.length);
    for (const name of names) expect(name).toMatch(/^[a-z][a-z0-9-]{0,40}$/);
  });
});

describe("pageMessageText", () => {
  it("says where the message came from", () => {
    expect(pageMessageText({ text: "a = FORMAT", title: "VCF quiz" })).toBe("From the page “VCF quiz”:\n\na = FORMAT");
    expect(pageMessageText({ text: "hi", title: "  " })).toBe("From a page:\n\nhi");
  });
});
