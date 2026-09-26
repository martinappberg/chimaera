import { describe, expect, it } from "vitest";
import {
  cropLayout,
  embedKind,
  frameWidth,
  isMissing,
  mediaUrl,
  parseSizeHint,
  rawUrl,
  splitTarget,
  type TargetInfo,
} from "./embed";

const file = (path: string, extra: Partial<TargetInfo> = {}): TargetInfo => ({
  path,
  kind: "file",
  size: 10,
  version: "1",
  mtime_ms: 1,
  mime: "application/octet-stream",
  ...extra,
});

describe("embedKind", () => {
  it("gives every file its body", () => {
    expect(embedKind(file("/a/plot.png"))).toBe("image");
    expect(embedKind(file("/a/paper.pdf"))).toBe("pdf");
    expect(embedKind(file("/a/report.html"))).toBe("html");
    expect(embedKind(file("/a/data.csv"))).toBe("table");
    expect(embedKind(file("/a/calls.vcf"))).toBe("table");
    expect(embedKind(file("/a/book.xlsx"))).toBe("xlsx");
    expect(embedKind(file("/a/run.mp4"))).toBe("video");
    expect(embedKind(file("/a/voice.wav"))).toBe("audio");
    expect(embedKind(file("/a/analysis.ipynb"))).toBe("notebook");
    expect(embedKind(file("/a/notes.md"))).toBe("markdown");
    expect(embedKind(file("/a/main.rs"))).toBe("code");
    expect(embedKind(file("/a/slurm-1.out"))).toBe("code");
    expect(embedKind(file("/a/archive.zip"))).toBe("file");
    expect(embedKind({ path: "/a/figs", kind: "dir" })).toBe("dir");
  });
});

describe("rawUrl", () => {
  it("addresses an HTML page by its name under the ticket", () => {
    expect(rawUrl(file("/r/My Report.html", { ticket: "t-1" }))).toBe("/raw/t-1/My%20Report.html");
    expect(rawUrl(file("/r/plot.png", { ticket: "t-2" }))).toBe("/raw/t-2");
    expect(rawUrl(file("/r/notes.md"))).toBeNull();
  });

  it("appends a media moment", () => {
    expect(mediaUrl("/raw/t", { start: 30, end: 45 })).toBe("/raw/t#t=30,45");
    expect(mediaUrl("/raw/t", { start: 2 })).toBe("/raw/t#t=2");
    expect(mediaUrl("/raw/t", undefined)).toBe("/raw/t");
  });
});

describe("targets and hints", () => {
  it("splits a target at its fragment", () => {
    expect(splitTarget("figs/a.png#xywh=1,2,3,4")).toEqual({ path: "figs/a.png", fragment: "xywh=1,2,3,4" });
    expect(splitTarget("a.png#")).toEqual({ path: "a.png", fragment: null });
    expect(splitTarget("a.png")).toEqual({ path: "a.png", fragment: null });
  });

  it("reads Obsidian's size hint from the alt text", () => {
    expect(parseSizeHint("umap|400")).toEqual({ alt: "umap", width: 400, height: null });
    expect(parseSizeHint("a | b|300x200")).toEqual({ alt: "a | b", width: 300, height: 200 });
    expect(parseSizeHint("plain caption")).toEqual({ alt: "plain caption", width: null, height: null });
    expect(parseSizeHint("|0")).toEqual({ alt: "", width: null, height: null });
  });

  it("tells a miss from a hit", () => {
    expect(isMissing({ missing: true })).toBe(true);
    expect(isMissing(file("/x"))).toBe(false);
  });
});

describe("cropLayout / frameWidth", () => {
  it("positions the whole picture inside a region frame", () => {
    const c = cropLayout({ w: 1000, h: 500 }, { x: 250, y: 100, w: 500, h: 250 });
    expect(c).toEqual({ aspect: 2, width: 200, height: 200, left: -50, top: -40 });
  });

  it("takes percent regions and clamps to the picture", () => {
    const c = cropLayout({ w: 200, h: 100 }, { x: 50, y: 0, w: 50, h: 100, percent: true });
    expect(c).toEqual({ aspect: 1, width: 200, height: 100, left: -100, top: -0 });
    const clamped = cropLayout({ w: 100, h: 100 }, { x: 80, y: 80, w: 50, h: 50 });
    expect(clamped?.aspect).toBe(1);
    expect(clamped?.width).toBe(500);
    expect(cropLayout({ w: 100, h: 100 }, { x: 200, y: 0, w: 10, h: 10 })).toBeNull();
  });

  it("reserves the final width before a byte loads", () => {
    expect(frameWidth({ w: 800, h: 400 }, 420)).toBe(800);
    expect(frameWidth({ w: 800, h: 1600 }, 420)).toBe(210);
    expect(frameWidth({ w: 800, h: 400 }, 420, 300)).toBe(300);
    expect(frameWidth({ w: 64, h: 64 }, 420)).toBe(64);
  });
});
