import { describe, expect, it } from "vitest";
import {
  frontmatterOf,
  isMarpFrontmatter,
  isMarpSource,
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
});
