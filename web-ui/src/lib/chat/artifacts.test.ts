import { describe, expect, it } from "vitest";
import {
  artifactMentions,
  artifactShape,
  chipLabels,
  fileStateAfter,
  isArtifactPath,
  namesFile,
  proseCovered,
  proseEmbedTargets,
  writtenDuring,
} from "./artifacts";

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

describe("artifactShape", () => {
  it("tells what is looked at from what is opened", () => {
    // Every image the previews render is a visual (favicon.ico included).
    for (const p of ["figs/umap.png", "fig.svg", "favicon.ico", "report.html", "paper.pdf", "clip.mp4", "talk.mp3"]) {
      expect(artifactShape(p), p).toBe("visual");
    }
    for (const p of ["notes.md", "out/summary.csv", "t.tsv.gz", "run.ipynb", "deck.pptx", "memo.docm", "calls.vcf"]) {
      expect(artifactShape(p), p).toBe("document");
    }
    for (const p of ["main.rs", "run.log", "flow.mmd", "config.json"]) {
      expect(artifactShape(p), p).toBeNull();
    }
  });
});

describe("prose embeds", () => {
  it("lists the local files the prose embeds, fragments off, URLs skipped", () => {
    const texts = [
      "Here it is:\n\n![umap](figs/umap.png)\n\nand page 3: ![p](<paper.pdf#page=3>)",
      "![again](figs/umap.png) ![web](https://x.test/a.png) ![abs](/home/me/proj/out/report.html)",
      "![spaced](<figs/my plot.png>)",
      // Quoted in code, an embed is text, not a card.
      "Use `![x](inline.png)` like so:\n\n```md\n![y](fenced.png)\n```\n",
    ];
    expect(proseEmbedTargets(texts)).toEqual([
      "figs/umap.png",
      "paper.pdf",
      "/home/me/proj/out/report.html",
      "figs/my plot.png",
    ]);
  });

  it("tells whether a written name refers to a file", () => {
    expect(namesFile("figs/umap.png", "/home/me/proj/figs/umap.png")).toBe(true);
    expect(namesFile("./figs/umap.png", "figs/umap.png")).toBe(true);
    expect(namesFile("notes.md", "./notes.md")).toBe(true);
    expect(namesFile("/home/me/proj/out/report.html", "/home/me/proj/out/report.html")).toBe(true);
    // A suffix match needs a directory boundary; an absolute name is one file.
    expect(namesFile("figs/umap.png", "/home/me/proj/oldfigs/umap.png")).toBe(false);
    expect(namesFile("figs/umap.png", "/home/me/proj/figs/xumap.png")).toBe(false);
    expect(namesFile("/home/me/proj/out/report.html", "/tmp/out/report.html")).toBe(false);
    expect(namesFile("notes.md", "other.md")).toBe(false);
  });

  it("a name covers the shallowest file it matches, never a family", () => {
    const paths = ["/p/docs/notes.md", "/p/notes.md", "/p/out/summary.csv", "/p/figs/umap.png"];
    expect([...proseCovered(paths, ["notes.md", "summary.csv"])]).toEqual(["/p/notes.md", "/p/out/summary.csv"]);
    expect([...proseCovered(paths, ["docs/notes.md"])]).toEqual(["/p/docs/notes.md"]);
    expect([...proseCovered(paths, ["/p/figs/umap.png", "missing.md"])]).toEqual(["/p/figs/umap.png"]);
    expect(proseCovered(paths, []).size).toBe(0);
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

describe("fileStateAfter", () => {
  const info = (mtime_ms: number | null) =>
    ({ path: "/p/a.md", kind: "file", size: 1, version: "v", mtime_ms, mime: "text/markdown" }) as const;
  it("tells present from changed-later from gone", () => {
    expect(fileStateAfter({ missing: true }, 20_000)).toBe("gone");
    expect(fileStateAfter(info(19_000), 20_000)).toBe("present");
    expect(fileStateAfter(info(22_000), 20_000)).toBe("present"); // clock slack
    expect(fileStateAfter(info(60_000), 20_000)).toBe("changed");
    expect(fileStateAfter(info(60_000), null)).toBe("present"); // turn still open
    expect(fileStateAfter(info(null), 20_000)).toBe("present");
  });
});

describe("chipLabels", () => {
  it("names files, widening only where names collide", () => {
    const labels = chipLabels(["/p/notes.md", "/p/a/README.md", "/p/b/README.md", "/p/out/summary.csv"]);
    expect([...labels.values()]).toEqual(["notes.md", "a/README.md", "b/README.md", "summary.csv"]);
  });
  it("keeps widening until distinct, and stops at the root", () => {
    const labels = chipLabels(["/x/a/docs/README.md", "/y/a/docs/README.md", "README.md"]);
    expect(labels.get("/x/a/docs/README.md")).toBe("x/a/docs/README.md");
    expect(labels.get("/y/a/docs/README.md")).toBe("y/a/docs/README.md");
    expect(labels.get("README.md")).toBe("README.md");
    expect(chipLabels([])).toEqual(new Map());
  });
});
