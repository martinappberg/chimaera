import { describe, expect, it } from "vitest";
import {
  frontmatterOf,
  isMarpFrontmatter,
  isMarpSource,
  PRINT_FIT_WIDTH,
  printIgnoresPageSize,
  relativeImages,
  rewriteImages,
  slideCount,
  slideDocument,
  slideSize,
} from "./marp";

describe("marp detection", () => {
  it("finds marp: true in frontmatter, with or without fences", () => {
    expect(isMarpFrontmatter("marp: true\ntheme: gaia")).toBe(true);
    expect(isMarpFrontmatter("---\ntitle: x\nmarp: true # deck\n---")).toBe(true);
    expect(isMarpFrontmatter("marp: 'true'")).toBe(true);
    expect(isMarpFrontmatter("marp: false")).toBe(false);
    expect(isMarpFrontmatter("notmarp: true")).toBe(false);
  });

  it("reads a source's leading block only", () => {
    expect(isMarpSource("---\nmarp: true\n---\n# Slide\n")).toBe(true);
    expect(isMarpSource("﻿---\r\nmarp: true\r\n---\r\n# Slide\r\n")).toBe(true);
    expect(isMarpSource("# Doc\n\n---\nmarp: true\n---\n")).toBe(false);
    expect(isMarpSource("---\nmarp: true\n")).toBe(false); // never closed
    expect(frontmatterOf("---\na: 1\n...\nbody")).toBe("a: 1");
  });
});

describe("image targets", () => {
  const deck = [
    "---",
    "marp: true",
    "---",
    "![bg left:40%](figs/umap.png)",
    "![w:200](<figs/a b.png> \"title\")",
    "![remote](https://example.com/x.png) ![data](data:image/png;base64,AAAA)",
    "`![code](not/this.png)`",
    "```",
    "![fenced](nor/this.png)",
    "```",
    "![again](figs/umap.png)",
  ].join("\n");

  it("lists relative targets outside code, once each", () => {
    expect(relativeImages(deck)).toEqual(["figs/umap.png", "figs/a b.png"]);
  });

  it("rewrites them and leaves everything else as written", () => {
    const out = rewriteImages(
      deck,
      new Map([
        ["figs/umap.png", "/raw/t1"],
        ["figs/a b.png", "/raw/t2"],
      ]),
    );
    expect(out).toContain("![bg left:40%](</raw/t1>)");
    expect(out).toContain('![w:200](</raw/t2> "title")');
    expect(out).toContain("![again](</raw/t1>)");
    expect(out).toContain("https://example.com/x.png");
    expect(out).toContain("`![code](not/this.png)`");
    expect(out).toContain("![fenced](nor/this.png)");
  });
});

describe("rendered deck helpers", () => {
  const html =
    '<div class="marpit"><svg data-marpit-svg="" viewBox="0 0 960 720"></svg><svg data-marpit-svg="" viewBox="0 0 960 720"></svg></div>';

  it("reads the slide size and count", () => {
    expect(slideSize(html)).toEqual({ w: 960, h: 720 });
    expect(slideSize("<div></div>")).toEqual({ w: 1280, h: 720 });
    expect(slideCount(html)).toBe(2);
  });

  it("builds a script-free document per layout", () => {
    const doc = slideDocument("section{color:red}", html, { w: 960, h: 720 }, "row", 90);
    expect(doc).toContain("flex-direction:row");
    expect(doc).toContain("gap:90px");
    expect(doc).not.toMatch(/<script/i);
    const print = slideDocument("", html, { w: 960, h: 720 }, "print");
    expect(print).toContain("@page{size:960px 720px");
  });

  it("overrides Marp's own print rules, which blank the PDF in WebKit", () => {
    const marpPrint =
      "@media print{html, body{break-inside:avoid-page}div.marpit > svg > foreignObject > section{break-before:page}" +
      "div.marpit > svg[data-marpit-svg]{display:block;height:100vh;width:100vw}}";
    const print = slideDocument(marpPrint, html, { w: 960, h: 720 }, "print");
    const after = print.slice(print.indexOf(marpPrint) + marpPrint.length);
    // Same selectors, later in the document: ours win the cascade.
    expect(after).toContain("div.marpit>svg[data-marpit-svg]{display:block;width:960px;height:720px;");
    expect(after).toContain("div.marpit>svg>foreignObject>section{break-before:auto;");
    expect(after).toMatch(/html,body\{[^}]*break-inside:auto/);
    expect(after).not.toContain("transform:");
  });

  it("scales printed slides by transform where the paper ignores @page size", () => {
    const print = slideDocument("", html, { w: 1280, h: 720 }, "print", 0, PRINT_FIT_WIDTH);
    expect(print).toContain(`transform:scale(${PRINT_FIT_WIDTH / 1280})`);
    // Height pulled in to the scaled 506px (720 − 214), centred at 900px wide.
    expect(print).toContain("margin:0 0 -214px max(0px,calc((100% - 900px) / 2))");
    // A slide already narrower than the fit width prints at its own size.
    expect(slideDocument("", html, { w: 800, h: 600 }, "print", 0, PRINT_FIT_WIDTH)).not.toContain("transform:");
  });

  it("knows which engines print on the panel's paper", () => {
    const safari =
      "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15";
    const wkwebview = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko)";
    const chrome =
      "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/153.0.0.0 Safari/537.36";
    const edge = `${chrome} Edg/153.0.0.0`;
    expect(printIgnoresPageSize(safari)).toBe(true);
    expect(printIgnoresPageSize(wkwebview)).toBe(true);
    expect(printIgnoresPageSize(chrome)).toBe(false);
    expect(printIgnoresPageSize(edge)).toBe(false);
    expect(printIgnoresPageSize("Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Gecko/20100101 Firefox/140.0")).toBe(false);
  });
});
