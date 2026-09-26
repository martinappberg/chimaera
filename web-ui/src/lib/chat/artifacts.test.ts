import { describe, expect, it } from "vitest";
import { artifactMentions, isArtifactPath, writtenDuring } from "./artifacts";

describe("isArtifactPath", () => {
  it("keeps outputs, not source code", () => {
    for (const p of ["figs/umap.png", "report.html", "paper.pdf", "notes.md", "t.tsv.gz", "run.ipynb", "a.mp4", "deck.pptx"]) {
      expect(isArtifactPath(p), p).toBe(true);
    }
    for (const p of ["main.rs", "plot.py", "Makefile", "data.json", "run.log"]) {
      expect(isArtifactPath(p), p).toBe(false);
    }
  });
});

describe("artifactMentions", () => {
  it("finds paths in commands, outputs and prose", () => {
    const texts = [
      "python scripts/plot.py --out figs/umap.png && open 'results/report.html'",
      "Saved figure to /home/me/proj/figs/umap.png.\nWrote `out/summary.csv`",
      "I've written **notes/findings.md** and the deck (slides.md).",
    ];
    expect(artifactMentions(texts)).toEqual([
      "figs/umap.png",
      "results/report.html",
      "/home/me/proj/figs/umap.png",
      "out/summary.csv",
      "notes/findings.md",
      "slides.md",
    ]);
  });

  it("skips URLs, hidden files, dupes and non-artifacts, and caps", () => {
    expect(
      artifactMentions([
        "see https://example.com/a.png and file:///x/b.pdf, .cache/c.png, ./d.png, ../e.pdf, d.png d.png, main.rs",
      ]),
    ).toEqual(["./d.png", "../e.pdf", "d.png"]);
    const many = Array.from({ length: 40 }, (_, i) => `f${i}.png`).join(" ");
    expect(artifactMentions([many], 5)).toHaveLength(5);
  });

  it("scans the head and tail of a long output", () => {
    const long = `start.png ${"x ".repeat(20_000)} middle.png ${"y ".repeat(20_000)} end.png`;
    expect(artifactMentions([long])).toEqual(["start.png", "end.png"]);
  });
});

describe("writtenDuring", () => {
  it("keeps files modified inside the turn, with slack", () => {
    expect(writtenDuring(10_000, 9_000, 20_000)).toBe(true);
    expect(writtenDuring(8_000, 9_000, 20_000)).toBe(true); // clock slack
    expect(writtenDuring(1_000, 9_000, 20_000)).toBe(false); // older: merely mentioned
    expect(writtenDuring(60_000, 9_000, 20_000)).toBe(false); // rewritten by a later turn
    expect(writtenDuring(60_000, 9_000, null)).toBe(true);
    expect(writtenDuring(null, 9_000, 20_000)).toBe(false);
    expect(writtenDuring(10_000, null, 20_000)).toBe(false);
  });
});
