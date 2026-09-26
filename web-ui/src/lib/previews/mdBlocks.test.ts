import { describe, expect, it } from "vitest";
import { EditorState, type TransactionSpec } from "@codemirror/state";
import { ensureSyntaxTree, syntaxTree } from "@codemirror/language";
import type { WidgetType } from "@codemirror/view";
import { markdownLanguageExt } from "./mdLive";
import { isFigureLine } from "./doc/live";
import { figureHeldAt, liveField, liveFocus } from "./mdBlocks";

const extensions = [markdownLanguageExt, liveField];

function fresh(doc: string, cursor: number, focused: boolean): EditorState {
  let s = EditorState.create({ doc, extensions, selection: { anchor: cursor } });
  ensureSyntaxTree(s, s.doc.length, 5000);
  s = s.update({ effects: liveFocus(focused) }).state;
  return s;
}

/** What the field says, comparable across states: segments, identities,
 *  reveal flags, and every decoration (range, kind, widget identity). */
function summary(s: EditorState) {
  const st = s.field(liveField);
  const deco: string[] = [];
  st.deco.between(0, s.doc.length, (from, to, value) => {
    const w = value.spec.widget as (WidgetType & { id?: string }) | undefined;
    // A revealed figure's drawing keeps the identity it was entered with
    // (history the oracle has not seen): compared by place only.
    const id = w?.id?.startsWith("V\u0000") === true ? "V" : w?.id;
    deco.push(`${from}-${to}:${id?.replace(/\u0000/g, "|") ?? "?"}`);
  });
  return {
    segs: st.segs.map((g) => `${g.kind}:${g.from}-${g.to}/${g.blockFrom}-${g.blockTo}:${g.names}`),
    keys: st.keys,
    revealed: st.revealed,
    deco,
  };
}

/** A tree's top-level nodes with their extents. */
function shape(s: EditorState): string {
  const out: string[] = [];
  for (let c = syntaxTree(s).topNode.firstChild; c !== null; c = c.nextSibling) out.push(`${c.name}:${c.from}-${c.to}`);
  return out.join(",");
}

/** A small deterministic generator. */
function rng(seed: number): () => number {
  let x = seed;
  return () => {
    x = (x * 1103515245 + 12345) & 0x7fffffff;
    return x / 0x7fffffff;
  };
}

const DOC = [
  "# Title",
  "",
  "Intro paragraph with **bold** and a [ref][r] link.",
  "Second line of it.",
  "",
  "- one",
  "- two",
  "  - nested",
  "- [ ] task",
  "",
  "```js",
  "const x = 1;",
  "```",
  "",
  "| a | b |",
  "|---|---|",
  "| 1 | 2 |",
  "",
  "> quoted",
  "> more",
  "",
  "$$",
  "x^2",
  "$$",
  "",
  "Closing paragraph.",
  "",
  "![plot](figs/plot.png)",
  "",
  "[r]: https://example.com",
  "",
  "Tail paragraph after the definition.",
  "",
].join("\n");

/** Equal headings (one id each: `same`, `same-1`, …) around short blocks,
 *  so most edits land between two of them. */
const DUPS =
  Array.from({ length: 10 }, (_, k) => `## Same\n\nword ${k}\n\n${k % 3 === 2 ? "> ## Same\n\n[d]: x\n\n" : ""}`).join("") +
  "end\n";

/** Frontmatter whose YAML opens a fence the editor's markdown parse runs
 *  to the end of the document. */
const FENCED_FM = "---\ntitle: x\nexample: |\n  ```\n---\n\n";

const INSERTS = ["x", " ", "\n", "\n\n", "- ", "```", "# ", "## ", "> ", "|", "**", "[", "---\n", "1. ", "[^1]", "<div>", "$$"];
/** Typing that keeps a document's blocks as they are (most of the time). */
const TYPING = ["x", " ", "ab"];

describe("the live field's incremental update", () => {
  it("matches a full recomputation after every edit", () => {
    let treeDiffs = 0;
    let steps = 0;
    const runs = [
      [7, true, DOC],
      [11, false, DOC],
      [23, true, DOC],
      [42, true, DOC],
      [13, true, DUPS, TYPING],
      [29, true, DUPS, TYPING],
      [5, true, FENCED_FM + DOC],
    ] as const;
    for (const [seed, focused, text, inserts = INSERTS] of runs) {
      const typing = inserts === TYPING;
      const next = rng(seed);
      let focus: boolean = focused;
      let state = fresh(text, 30, focus);
      for (let step = 0; step < 400; step++) {
        steps++;
        const len = state.doc.length;
        const at = Math.floor(next() * (len + 1));
        let spec: TransactionSpec;
        const roll = next();
        if (roll < 0.2) {
          // A cursor move or a selection across blocks, no edit.
          const head = Math.min(len, at + Math.floor(next() * 3) * Math.floor(next() * 60));
          spec = { selection: { anchor: at, head } };
        } else if (roll < 0.25) {
          focus = !focus;
          spec = { effects: liveFocus(focus) };
        } else if (roll < 0.5 && len > 0) {
          const to = Math.min(len, at + 1 + (typing ? 0 : Math.floor(next() * 12)));
          spec = { changes: { from: at, to }, selection: { anchor: at } };
        } else {
          const ins = inserts[Math.floor(next() * inserts.length)];
          spec = { changes: { from: at, insert: ins }, selection: { anchor: at + ins.length } };
        }
        state = state.update(spec).state;
        ensureSyntaxTree(state, state.doc.length, 5000);
        const sel = state.selection.main;
        let base = fresh(state.doc.toString(), sel.head, focus);
        if (!sel.empty) base = base.update({ selection: { anchor: sel.anchor, head: sel.head } }).state;
        // Parse completion may land a transaction later: settle both.
        state = state.update({}).state;
        // The oracle is the same computation from scratch over the same
        // tree; where lezer's incremental reparse and a fresh parse disagree
        // (a `$$` block's extent, rarely), the trees are not comparable.
        if (shape(state) !== shape(base)) {
          treeDiffs++;
          continue;
        }
        const want = summary(base);
        const got = summary(state);
        const where = `seed ${seed} step ${step}`;
        expect(got.segs, where).toEqual(want.segs);
        expect(got.keys, where).toEqual(want.keys);
        expect(got.revealed, where).toEqual(want.revealed);
        expect(got.deco, where).toEqual(want.deco);
      }
    }
    // Nearly every step compares.
    expect(treeDiffs).toBeLessThan(steps / 10);
  });

  it("keeps equal headings apart when typing between them", () => {
    const text = "# Doc\n\n## Same\n\nbetween\n\n## Same\n\n[d]: x\n\nmiddle\n\n[d]: x\n\n## Same\n\nend\n";
    for (const word of ["between", "middle"]) {
      let state = fresh(text, text.indexOf(word), true);
      for (const ch of "typed ") {
        const at = state.selection.main.head;
        state = state.update({ changes: { from: at, insert: ch }, selection: { anchor: at + 1 } }).state;
        const want = summary(fresh(state.doc.toString(), state.selection.main.head, true));
        const got = summary(state);
        expect(got.keys, word).toEqual(want.keys);
        expect(got.deco, word).toEqual(want.deco);
      }
      // Three headings, three ids.
      const ids = state.field(liveField).keys.filter((k) => k.startsWith("ATXHeading2"));
      expect(new Set(ids).size).toBe(3);
    }
    // One edit that swaps two equal headings' places (a quoted and a bare
    // one): each keeps the id of its place, not its twin's.
    const swap = "# Doc\n\n## Same\n\n> ## Same\n\nx\n\n## Same\n\nend\n";
    let state = fresh(swap, swap.indexOf("x"), true);
    const from = swap.indexOf("> ## Same");
    const to = swap.indexOf("\n\nend");
    state = state.update({
      changes: { from, to, insert: "## Same\n\nx\n\n> ## Same" },
      selection: { anchor: from + 10 },
    }).state;
    const want = summary(fresh(state.doc.toString(), state.selection.main.head, true));
    expect(summary(state).keys).toEqual(want.keys);
    expect(summary(state).deco).toEqual(want.deco);
  });

  it("draws the body from its own parse when a fence opened in the frontmatter runs on", () => {
    const text = `${FENCED_FM}# Body\n\npara one\n\n- item\n`;
    let state = fresh(text, text.indexOf("para"), false);
    const kinds = (s: EditorState) => s.field(liveField).segs.map((g) => `${g.kind}:${g.names}`);
    const want = ["front:Frontmatter", "block:ATXHeading1", "block:Paragraph", "block:BulletList"];
    expect(kinds(state)).toEqual(want);
    // The properties panel covers the frontmatter's lines and no more.
    const panel = (s: EditorState): number => {
      let to = -1;
      s.field(liveField).deco.between(0, s.doc.length, (from, end, value) => {
        const w = value.spec.widget as (WidgetType & { id?: string }) | undefined;
        if (w?.id?.startsWith("P\u0000") === true && from === 0) to = end;
      });
      return to;
    };
    expect(panel(state)).toBeLessThan(text.indexOf("# Body"));
    // Typing in the body: still drawn from the body's own parse.
    const at = state.doc.toString().indexOf("one");
    state = state.update({ selection: { anchor: at }, effects: liveFocus(true) }).state;
    state = state.update({ changes: { from: at, insert: "and " }, selection: { anchor: at + 4 } }).state;
    expect(kinds(state)).toEqual(want);
    expect(state.field(liveField).revealed).toEqual([false, false, true, false]);
    expect(panel(state)).toBeLessThan(state.doc.toString().indexOf("# Body"));
  });

  it("keeps a revealed figure drawn as entered until the cursor leaves", () => {
    const figure = DOC.indexOf("![plot]");
    let state = fresh(DOC, figure + 2, true);
    const preview = (s: EditorState): string | null => {
      let id: string | null = null;
      s.field(liveField).deco.between(0, s.doc.length, (_from, _to, value) => {
        const w = value.spec.widget as (WidgetType & { id?: string }) | undefined;
        if (w?.id?.startsWith("V\u0000") === true) id = w.id;
      });
      return id;
    };
    const entered = preview(state);
    expect(entered).toContain("![plot](figs/plot.png)");
    // Retyping the path: the drawing stays the one entered.
    const path = state.doc.toString().indexOf("plot.png)");
    state = state.update({ changes: { from: path, to: path + 4, insert: "chart" }, selection: { anchor: path + 5 } }).state;
    expect(preview(state)).toBe(entered);
    // Left: the block renders its new text; no drawing is held.
    state = state.update({ selection: { anchor: 0 } }).state;
    expect(preview(state)).toBeNull();
    // Entered again: drawn as it now reads.
    state = state.update({ selection: { anchor: path + 2 } }).state;
    expect(preview(state)).toContain("figs/chart.png");
  });

  it("holds a figure under its line only for a paragraph that is the one image", () => {
    const text = [
      "Text above",
      "![inline](figs/a.png)",
      "and below.",
      "",
      "- item",
      "  ![listed](figs/b.png)",
      "",
      "![figure](figs/c.png)",
      "",
    ].join("\n");
    const line = (s: EditorState, src: string) => s.doc.line(s.doc.toString().split("\n").indexOf(src) + 1);
    const at = (s: EditorState, src: string) => line(s, src).from;
    // Each image line reads as a figure's source on its own…
    for (const src of ["![inline](figs/a.png)", "  ![listed](figs/b.png)", "![figure](figs/c.png)"])
      expect(isFigureLine(src)).toBe(true);
    // …but revealed with the cursor on another line of its block, it is
    // drawn inline (a larger block holds no figure).
    let state = fresh(text, at(fresh(text, 0, true), "Text above"), true);
    expect(figureHeldAt(state, at(state, "![inline](figs/a.png)"))).toBe(false);
    state = state.update({ selection: { anchor: at(state, "- item") } }).state;
    expect(figureHeldAt(state, at(state, "  ![listed](figs/b.png)"))).toBe(false);
    // A one-image paragraph revealed: its figure stays drawn below it.
    state = state.update({ selection: { anchor: at(state, "![figure](figs/c.png)") + 3 } }).state;
    expect(figureHeldAt(state, at(state, "![figure](figs/c.png)"))).toBe(true);
    // Unfocused, nothing is revealed and nothing held.
    state = state.update({ effects: liveFocus(false) }).state;
    expect(figureHeldAt(state, at(state, "![figure](figs/c.png)"))).toBe(false);
  });

  it("keeps an unchanged block's widget across an edit elsewhere", () => {
    let state = fresh(DOC, 0, true);
    const widgetAt = (s: EditorState, pos: number) => {
      let w: unknown = null;
      s.field(liveField).deco.between(pos, pos, (from, _to, value) => {
        if (from === pos) w = value.spec.widget;
      });
      return w;
    };
    const closing = state.doc.toString().indexOf("Closing");
    const before = widgetAt(state, closing);
    expect(before).not.toBeNull();
    // Type in the intro paragraph (revealed: the cursor is there).
    const intro = state.doc.toString().indexOf("Second line");
    state = state.update({ selection: { anchor: intro } }).state;
    state = state.update({ changes: { from: intro, insert: "typed " }, selection: { anchor: intro + 6 } }).state;
    expect(widgetAt(state, closing + 6)).toBe(before);
  });
});
