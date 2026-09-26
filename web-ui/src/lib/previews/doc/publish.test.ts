import { describe, expect, it } from "vitest";
import {
  applyEdits,
  commonDir,
  docTargets,
  htmlPage,
  isInside,
  localTarget,
  planBundle,
  planInline,
  relPath,
  stemOf,
  titleBlock,
  type DocTarget,
  type FoundTarget,
} from "./publish";

const hrefs = (src: string) => docTargets(src).map((t) => `${t.kind}:${t.href}`);

describe("the files a document names", () => {
  it("reads links, images, definitions, wikilinks and raw HTML in order", () => {
    const src = [
      "---",
      "title: [not](a-link.md)",
      "---",
      "# Title",
      "",
      "See [methods](methods.md#setup) and ![plot](figs/plot.png).",
      "",
      "[![badge](figs/badge.svg)](https://example.com)",
      "",
      "[ref]: data/table.csv",
      "",
      "A [[note]] and ![[deck.pdf#page=2]] by name.",
      "",
      '<img src="figs/raw.png" alt="x"> and <a href=\'other.md\'>o</a>',
    ].join("\n");
    expect(hrefs(src)).toEqual([
      "link:methods.md#setup",
      "image:figs/plot.png",
      "link:https://example.com",
      "image:figs/badge.svg",
      "link:data/table.csv",
      "wikilink:note.md",
      "wikilink:deck.pdf#page=2",
      "html:figs/raw.png",
      "html:other.md",
    ]);
  });

  it("skips code, math and comments", () => {
    const src = "`[x](a.md)`\n\n```\n![y](b.png)\n```\n\n$[z](c.md)$\n\n<!-- [w](d.md) -->\n";
    expect(docTargets(src)).toEqual([]);
  });

  it("escapes destinations as the renderer's hrefs, spans on the source", () => {
    const src = "[a](<my file.md>) [b](/abs/x.png)";
    const [a, b] = docTargets(src);
    expect(a.href).toBe("my%20file.md");
    expect(src.slice(a.span!.from, a.span!.to)).toBe("<my file.md>");
    expect(src.slice(b.span!.from, b.span!.to)).toBe("/abs/x.png");
  });

  it("maps spans back through CRLF line ends and frontmatter", () => {
    const src = "---\r\ntitle: t\r\n---\r\n\r\nOne\r\ntwo [x](/p/x.md)\r\n\r\n<img src='/p/y.png'>\r\n";
    const spans = docTargets(src).map((t) => src.slice(t.span!.from, t.span!.to));
    expect(spans).toEqual(["/p/x.md", "/p/y.png"]);
  });

  it("tells local targets from web, mail and in-page ones", () => {
    expect(localTarget("figs/a%20b.png#xywh=0,0,1,1")).toEqual({ path: "figs/a b.png", fragment: "xywh=0,0,1,1" });
    expect(localTarget("/abs/x.md")).toEqual({ path: "/abs/x.md", fragment: null });
    expect(localTarget("https://example.com/x.png")).toBeNull();
    expect(localTarget("www.example.com")).toBeNull();
    expect(localTarget("mailto:a@b.c")).toBeNull();
    expect(localTarget("#heading")).toBeNull();
    expect(localTarget("data:image/png;base64,AA")).toBeNull();
  });
});

describe("paths", () => {
  it("relates, contains and joins", () => {
    expect(relPath("/ws/docs", "/ws/docs/a.md")).toBe("a.md");
    expect(relPath("/ws/docs", "/ws/data/x.csv")).toBe("../data/x.csv");
    expect(relPath("/ws", "/ws")).toBe(".");
    expect(isInside("/ws/a/b", "/ws")).toBe(true);
    expect(isInside("/ws", "/ws/")).toBe(true);
    expect(isInside("/wsx/a", "/ws")).toBe(false);
    expect(isInside("/x", "/")).toBe(true);
    expect(commonDir(["/ws/docs", "/ws/data/raw", "/ws/docs/figs"])).toBe("/ws");
    expect(commonDir(["/a", "/b"])).toBe("/");
    expect(stemOf("/ws/QC report.v2.md")).toBe("QC report.v2");
    expect(stemOf("/ws/a:b.md")).toBe("a-b");
  });
});

describe("the bundle", () => {
  const target = (href: string, span: DocTarget["span"] = null, kind: DocTarget["kind"] = "link"): DocTarget => ({
    href,
    span,
    kind,
    byName: kind === "wikilink",
  });
  const found = (t: DocTarget, written: string, real: string, size = 10, kind: "file" | "dir" = "file"): FoundTarget => ({
    target: t,
    written,
    fragment: localTarget(t.href)?.fragment ?? null,
    real,
    kind,
    size,
  });

  it("keeps the folder structure the links name, under one folder", () => {
    const plan = planBundle("/ws/docs/report.md", "/ws", [
      found(target("figs/plot.png", null, "image"), "figs/plot.png", "/ws/docs/figs/plot.png"),
      found(target("../data/x.csv#row=1-5"), "../data/x.csv", "/ws/data/x.csv"),
      // A symlink: placed where the link names it, read from where it points.
      found(target("latest.png", null, "image"), "latest.png", "/ws/runs/42/plot.png"),
    ]);
    expect(plan.root).toBe("/ws");
    expect(plan.doc).toBe("report/docs/report.md");
    expect(plan.files).toEqual([
      { entry: "report/docs/figs/plot.png", real: "/ws/docs/figs/plot.png", size: 10 },
      { entry: "report/data/x.csv", real: "/ws/data/x.csv", size: 10 },
      { entry: "report/docs/latest.png", real: "/ws/runs/42/plot.png", size: 10 },
    ]);
    expect(plan.rewrites).toEqual([]);
    expect(plan.bytes).toBe(30);
  });

  it("rewrites absolute paths to relative ones, fragment kept", () => {
    const src = "[m](/ws/docs/methods.md#setup) and ![p](/ws/figs/p.png)";
    const [m, p] = docTargets(src);
    const plan = planBundle("/ws/docs/report.md", "/ws", [
      found(m, "/ws/docs/methods.md", "/ws/docs/methods.md"),
      found(p, "/ws/figs/p.png", "/ws/figs/p.png"),
    ]);
    expect(applyEdits(src, plan.rewrites)).toBe("[m](methods.md#setup) and ![p](../figs/p.png)");
    expect(plan.files.map((f) => f.entry)).toEqual(["report/docs/methods.md", "report/figs/p.png"]);
  });

  it("places a wikilink's note where it is, and never rewrites it", () => {
    const plan = planBundle("/ws/a/doc.md", "/ws", [found(target("note.md", null, "wikilink"), "note.md", "/ws/b/note.md")]);
    expect(plan.files.map((f) => f.entry)).toEqual(["doc/b/note.md"]);
    expect(plan.rewrites).toEqual([]);
  });

  it("leaves out folders, files outside the workspace, the document itself and repeats", () => {
    const plan = planBundle(
      "/ws/doc.md",
      "/ws",
      [
        found(target("../../etc/passwd"), "../../etc/passwd", "/etc/passwd"),
        found(target("data/"), "data/", "/ws/data", 0, "dir"),
        found(target("doc.md#top"), "doc.md", "/ws/doc.md"),
        found(target("a.png"), "a.png", "/ws/a.png"),
        found(target("./a.png"), "./a.png", "/ws/a.png"),
      ],
      [target("gone.md")],
    );
    expect(plan.files.map((f) => f.entry)).toEqual(["doc/a.png"]);
    expect(plan.skipped).toEqual([
      { href: "gone.md", why: "missing" },
      { href: "../../etc/passwd", why: "outside" },
      { href: "data/", why: "folder" },
    ]);
  });

  it("stops at the caps and says so", () => {
    const many = ["a", "b", "c", "d"].map((n) => found(target(`${n}.bin`), `${n}.bin`, `/ws/${n}.bin`, 40));
    const byBytes = planBundle("/ws/doc.md", "/ws", many, [], { files: 10, bytes: 100 });
    expect(byBytes.files.map((f) => f.entry)).toEqual(["doc/a.bin", "doc/b.bin"]);
    expect(byBytes.skipped.map((s) => s.why)).toEqual(["cap", "cap"]);
    const byCount = planBundle("/ws/doc.md", "/ws", many, [], { files: 3, bytes: 1e9 });
    expect(byCount.files).toHaveLength(3);
  });
});

describe("inlining pictures", () => {
  it("inlines in document order until the cap, then links", () => {
    const items = [{ size: 30 }, { size: 50 }, { size: 30 }, { size: 20 }];
    const plan = planInline(items, 100);
    expect(plan.inline).toEqual([items[0], items[1], items[3]]);
    expect(plan.over).toEqual([items[2]]);
    expect(plan.bytes).toBe(100);
  });
});

describe("the page", () => {
  it("takes the title block from the frontmatter, without repeating the first heading", () => {
    const fm = "title: QC report, batch 7\nsummary: One-sentence abstract.\nupdated: 2026-09-25\nstatus: draft";
    expect(titleBlock(fm, "QC report,  batch 7", "/ws/qc.md")).toEqual({
      title: "QC report, batch 7",
      heading: null,
      summary: "One-sentence abstract.",
      updated: "2026-09-25",
    });
    expect(titleBlock(fm, "Results", "/ws/qc.md").heading).toBe("QC report, batch 7");
    expect(titleBlock(null, "Results", "/ws/qc.md")).toEqual({ title: "Results", heading: null, summary: null, updated: null });
    expect(titleBlock(null, null, "/ws/qc.md").title).toBe("qc");
  });

  it("escapes the block and forbids loading and scripts", () => {
    const page = htmlPage({ title: "<x>", heading: "a & b", summary: null, updated: "today" }, "p{}", "<p>hi</p>");
    expect(page).toContain("<title>&lt;x&gt;</title>");
    expect(page).toContain('<h1 class="doc-title">a &amp; b</h1>');
    expect(page).toContain('<p class="doc-updated">Updated today</p>');
    expect(page).toContain("default-src 'none'; img-src data:");
    expect(page).not.toContain("<script");
  });
});
