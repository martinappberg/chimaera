import { describe, expect, it } from "vitest";
import { encodePageTheme, PAGE_THEME_TOKENS } from "./pageTheme";
import { pageMessageText, splitPageMessage, writtenDuringChat, WRITTEN_SLACK_MS } from "./pageMessage";

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
  it("leads with the page the message came from, on one line", () => {
    expect(pageMessageText({ text: "a = FORMAT", title: "VCF quiz" })).toBe("[from the page “VCF quiz”] a = FORMAT");
    expect(pageMessageText({ text: "hi", title: "  " })).toBe("[from a page] hi");
  });

  it("splits back into the page and the words, whatever the page called itself", () => {
    for (const [title, text] of [
      ["TP53 variants", "Tell me about chr17:7671909."],
      ["Quiz ] “tricky”", "line one\nline two"],
      ["", "untitled"],
    ]) {
      const split = splitPageMessage(pageMessageText({ title, text }));
      expect(split?.body).toBe(text);
      expect(split?.title).toBe(title.replace(/[\]”]/g, "").trim());
    }
    expect(splitPageMessage("an ordinary message")).toBeNull();
  });
});

describe("writtenDuringChat", () => {
  const started = 1_800_000_000_000;
  it("counts a file written while the chat ran, within the clocks' slack", () => {
    expect(writtenDuringChat(started + 60_000, started)).toBe(true);
    expect(writtenDuringChat(started - WRITTEN_SLACK_MS + 1, started)).toBe(true);
  });
  it("refuses an older file, and an unknown time", () => {
    expect(writtenDuringChat(started - 86_400_000, started)).toBe(false);
    expect(writtenDuringChat(null, started)).toBe(false);
  });
});
