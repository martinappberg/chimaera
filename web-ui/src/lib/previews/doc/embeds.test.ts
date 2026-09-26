import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  resolveTargets: vi.fn(),
  fsValidate: vi.fn(),
}));

vi.mock("../../shared/embed/embed", async (orig) => ({
  ...(await orig<typeof import("../../shared/embed/embed")>()),
  resolveTargets: mocks.resolveTargets,
}));
vi.mock("../files", async (orig) => ({
  ...(await orig<typeof import("../files")>()),
  fsValidate: mocks.fsValidate,
}));

import { DocEmbeds, embedTargets, refKey } from "./embeds";
import { documentContext } from "./model";
import { embedSpecOf, soleEmbed } from "./render";
import { isFigureLine } from "./live";
import type { Inline } from "../mdTable";

const image = (url: string, alt = ""): Inline => ({ kind: "image", url, alt, title: null, source: `![${alt}](${url})` });
const wiki = (target: string, heading: string | null = null, alias: string | null = null, embed = true): Inline => ({
  kind: "wikilink",
  target,
  heading,
  alias,
  embed,
  source: `${embed ? "!" : ""}[[${target}]]`,
});

describe("what an image reference names", () => {
  it("reads a path, its fragment, and whether it is a picture", () => {
    expect(embedSpecOf(image("figs/plot.png", "plot|400"))).toEqual({
      target: "figs/plot.png",
      byName: false,
      alt: "plot|400",
      image: true,
    });
    expect(embedSpecOf(image("paper.pdf#page=3"))).toMatchObject({ target: "paper.pdf#page=3", image: false });
    // Escaped as an href is (the daemon decodes it).
    expect(embedSpecOf(image("my figs/a b.png"))?.target).toBe("my%20figs/a%20b.png");
    expect(embedSpecOf(image("fig.PNG?raw=1"))?.image).toBe(true);
  });

  it("names nothing on this host for web and data URLs or an anchor", () => {
    expect(embedSpecOf(image("https://example.com/x.png"))).toBeNull();
    expect(embedSpecOf(image("data:image/png;base64,AAAA"))).toBeNull();
    expect(embedSpecOf(image("//cdn.example.com/x.png"))).toBeNull();
    expect(embedSpecOf(image("#section"))).toBeNull();
    expect(embedSpecOf(image(""))).toBeNull();
    expect(embedSpecOf(image("javascript:alert(1)"))).toBeNull();
  });

  it("reads `![[name]]` as Obsidian does: .md for a bare name, the fragment as written", () => {
    expect(embedSpecOf(wiki("plot.png", null, "400"))).toEqual({ target: "plot.png", byName: true, alt: "400", image: true });
    expect(embedSpecOf(wiki("notes", "Next Steps"))).toMatchObject({ target: "notes.md#Next%20Steps", image: false });
    expect(embedSpecOf(wiki("paper.pdf", "page=2"))?.target).toBe("paper.pdf#page=2");
    expect(embedSpecOf(wiki("notes", null, null, false))).toBeNull();
    expect(embedSpecOf(wiki(""))).toBeNull();
  });

  it("makes a block only of a paragraph that is one reference", () => {
    expect(soleEmbed([image("a.png")])?.target).toBe("a.png");
    expect(soleEmbed([{ kind: "text", text: " " }, wiki("a.pdf"), { kind: "text", text: "\n" }])?.target).toBe("a.pdf");
    expect(soleEmbed([{ kind: "text", text: "see " }, image("a.png")])).toBeNull();
    expect(soleEmbed([image("a.png"), image("b.png")])).toBeNull();
    expect(soleEmbed([image("https://x.org/a.png")])).toBeNull();
    expect(soleEmbed([])).toBeNull();
  });

  it("knows a figure's source line", () => {
    expect(isFigureLine("![plot](figs/plot.png)")).toBe(true);
    expect(isFigureLine('![a \\] b](x.png "title")')).toBe(true);
    expect(isFigureLine("![[plot.png|400]]  ")).toBe(true);
    expect(isFigureLine("see ![plot](figs/plot.png)")).toBe(false);
    expect(isFigureLine("![a](x.png) ![b](y.png)")).toBe(false);
    expect(isFigureLine("![ref][label]")).toBe(false);
    expect(isFigureLine("    ![indented](code.png)")).toBe(false);
  });
});

describe("a document's references", () => {
  it("collects every image-shaped reference once, outside code", () => {
    const text = [
      "# T",
      "",
      "Inline ![a](figs/a.png) and ![[b.png]] and a [link](c.md).",
      "",
      "![a](figs/a.png)",
      "",
      "- item ![c](c.pdf#page=2)",
      "",
      "> ![q][ref]",
      "",
      "```",
      "![not](in-code.png)",
      "```",
      "",
      "`![not](inline-code.png)` ![web](https://example.com/w.png)",
      "",
      "| x |",
      "|---|",
      "| ![t](cell.svg) |",
      "",
      "[ref]: quoted.png",
    ].join("\n");
    const cx = documentContext(text);
    const got = embedTargets(cx.tree, cx.doc, cx.inline).map((r) => `${r.byName ? "name:" : ""}${r.target}`);
    expect(got).toEqual(["figs/a.png", "name:b.png", "c.pdf#page=2", "quoted.png", "cell.svg"]);
  });
});

describe("the document's answers", () => {
  const ctx = () => ({ wsRoot: "/ws", workspaceId: "w-1" });
  const hit = (path: string, version = "v1") => ({
    path,
    kind: "file" as const,
    size: 10,
    version,
    mtime_ms: 0,
    mime: "image/png",
    ticket: `t-${version}`,
  });
  beforeEach(() => {
    vi.useFakeTimers();
    mocks.resolveTargets.mockReset();
    mocks.fsValidate.mockReset();
  });
  afterEach(() => vi.useRealTimers());

  it("asks for the whole set in one request, against the document's folder and the workspace", async () => {
    mocks.resolveTargets.mockResolvedValue({ "a.png": hit("/ws/docs/a.png"), "b.pdf": { missing: true } });
    const e = new DocEmbeds("/ws/docs/doc.md", ctx);
    const a = e.ask({ target: "a.png", byName: false });
    e.sync([
      { target: "a.png", byName: false },
      { target: "b.pdf", byName: false },
    ]);
    await vi.advanceTimersByTimeAsync(20);
    expect(mocks.resolveTargets).toHaveBeenCalledTimes(1);
    expect(mocks.resolveTargets).toHaveBeenCalledWith(["a.png", "b.pdf"], "/ws/docs", { bases: ["/ws"], workspaceId: "w-1" });
    expect(await a).toMatchObject({ path: "/ws/docs/a.png" });
    expect(e.answer({ target: "b.pdf", byName: false })).toEqual({ missing: true });
    // The same set again asks nothing; a changed set asks again, whole.
    e.sync([
      { target: "b.pdf", byName: false },
      { target: "a.png", byName: false },
    ]);
    await vi.advanceTimersByTimeAsync(20);
    expect(mocks.resolveTargets).toHaveBeenCalledTimes(1);
    e.sync([{ target: "a.png", byName: false }]);
    await vi.advanceTimersByTimeAsync(20);
    expect(mocks.resolveTargets).toHaveBeenCalledTimes(2);
  });

  it("tells subscribers only what changed", async () => {
    mocks.resolveTargets.mockResolvedValueOnce({ "a.png": hit("/ws/a.png", "v1") });
    const e = new DocEmbeds("/ws/doc.md", ctx);
    const seen: string[] = [];
    e.subscribe((key, a) => seen.push(`${key}:${"missing" in a ? "missing" : a.version}`));
    e.sync([{ target: "a.png", byName: false }]);
    await vi.advanceTimersByTimeAsync(20);
    mocks.resolveTargets.mockResolvedValueOnce({ "a.png": hit("/ws/a.png", "v1") });
    e.refresh();
    await vi.advanceTimersByTimeAsync(20);
    mocks.resolveTargets.mockResolvedValueOnce({ "a.png": hit("/ws/a.png", "v2") });
    e.refresh();
    await vi.advanceTimersByTimeAsync(20);
    expect(seen).toEqual([`${refKey({ target: "a.png", byName: false })}:v1`, `${refKey({ target: "a.png", byName: false })}:v2`]);
  });

  it("tells subscribers an expired answer asked again, even when the daemon renewed the same ticket", async () => {
    const ref = { target: "a.png", byName: false };
    mocks.resolveTargets.mockResolvedValue({ "a.png": hit("/ws/a.png", "v1") });
    const e = new DocEmbeds("/ws/doc.md", ctx);
    const seen: string[] = [];
    e.subscribe((_key, a) => seen.push("missing" in a ? "missing" : a.ticket ?? ""));
    e.sync([ref]);
    await vi.advanceTimersByTimeAsync(20);
    expect(seen).toEqual(["t-v1"]);
    // Aging, not expired: drawn from, asked again, unchanged — nobody told.
    await vi.advanceTimersByTimeAsync(2 * 60_000);
    expect(e.answer(ref)).toMatchObject({ ticket: "t-v1" });
    await vi.advanceTimersByTimeAsync(20);
    expect(seen).toEqual(["t-v1"]);
    // Expired: an image drawn now finds no answer, and waits on the ask.
    await vi.advanceTimersByTimeAsync(9 * 60_000);
    expect(e.answer(ref)).toBeUndefined();
    void e.ask(ref);
    await vi.advanceTimersByTimeAsync(20);
    expect(seen).toEqual(["t-v1", "t-v1"]);
    expect(e.answer(ref)).toMatchObject({ ticket: "t-v1" });
  });

  it("acts on a file at once with a fresh answer, and after asking again with an expired one", async () => {
    const ref = { target: "a.png", byName: false };
    const gone = { target: "gone.png", byName: false };
    mocks.resolveTargets.mockResolvedValue({ "a.png": hit("/ws/a.png", "v1"), "gone.png": { missing: true } });
    const e = new DocEmbeds("/ws/doc.md", ctx);
    const opened: string[] = [];
    const open = (a: { path: string }) => void opened.push(a.path);
    // Never answered: asked, then acted on.
    expect(e.use(ref, open)).toBe(true);
    expect(opened).toEqual([]);
    await vi.advanceTimersByTimeAsync(20);
    expect(opened).toEqual(["/ws/a.png"]);
    // Fresh: at once.
    expect(e.use(ref, open)).toBe(true);
    expect(opened).toEqual(["/ws/a.png", "/ws/a.png"]);
    // Expired (the same ticket renewed): asked again, then acted on.
    await vi.advanceTimersByTimeAsync(9 * 60_000);
    expect(e.use(ref, open)).toBe(true);
    expect(opened).toHaveLength(2);
    await vi.advanceTimersByTimeAsync(20);
    expect(opened).toEqual(["/ws/a.png", "/ws/a.png", "/ws/a.png"]);
    // Known missing: nothing to act on, and the caller is told so.
    void e.ask(gone);
    await vi.advanceTimersByTimeAsync(20);
    expect(e.use(gone, open)).toBe(false);
    expect(opened).toHaveLength(3);
  });

  it("finds an `![[name]]` missing beside the document by name", async () => {
    mocks.resolveTargets
      .mockResolvedValueOnce({ "plot.png": { missing: true }, "gone.png": { missing: true } })
      .mockResolvedValueOnce({ "/ws/figs/plot.png": hit("/ws/figs/plot.png") });
    mocks.fsValidate.mockResolvedValue({ valid: { "plot.png": { path: "/ws/figs/plot.png", kind: "file" } }, ambiguous: {}, unchecked: [] });
    const e = new DocEmbeds("/ws/doc.md", ctx);
    const plot = e.ask({ target: "plot.png", byName: true });
    const gone = e.ask({ target: "gone.png", byName: true });
    await vi.advanceTimersByTimeAsync(20);
    expect(await plot).toMatchObject({ path: "/ws/figs/plot.png" });
    expect(await gone).toEqual({ missing: true });
    expect(mocks.fsValidate).toHaveBeenCalledWith(["plot.png", "gone.png"], "/ws", "w-1");
  });

  it("answers unknown, not missing, when the daemon cannot be reached", async () => {
    mocks.resolveTargets.mockRejectedValue(new Error("offline"));
    const e = new DocEmbeds("/ws/doc.md", ctx);
    const a = e.ask({ target: "a.png", byName: false });
    await vi.advanceTimersByTimeAsync(20);
    expect(await a).toBeNull();
    expect(e.answer({ target: "a.png", byName: false })).toBeUndefined();
  });

  it("stops asking once disposed", async () => {
    const e = new DocEmbeds("/ws/doc.md", ctx);
    const a = e.ask({ target: "a.png", byName: false });
    e.dispose();
    expect(await a).toBeNull();
    await vi.advanceTimersByTimeAsync(20);
    expect(mocks.resolveTargets).not.toHaveBeenCalled();
  });
});
