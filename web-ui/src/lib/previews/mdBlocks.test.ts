import { describe, expect, it } from "vitest";
import { EditorState, type TransactionSpec } from "@codemirror/state";
import { ensureSyntaxTree, syntaxTree } from "@codemirror/language";
import type { WidgetType } from "@codemirror/view";
import { markdownLanguageExt } from "./mdLive";
import { liveField, liveFocus } from "./mdBlocks";

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

const INSERTS = ["x", " ", "\n", "\n\n", "- ", "```", "# ", "## ", "> ", "|", "**", "[", "---\n", "1. ", "[^1]", "<div>", "$$"];

describe("the live field's incremental update", () => {
  it("matches a full recomputation after every edit", () => {
    let treeDiffs = 0;
    let steps = 0;
    for (const [seed, focused] of [[7, true], [11, false], [23, true], [42, true]] as const) {
      const next = rng(seed);
      let focus: boolean = focused;
      let state = fresh(DOC, 30, focus);
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
          const to = Math.min(len, at + 1 + Math.floor(next() * 12));
          spec = { changes: { from: at, to }, selection: { anchor: at } };
        } else {
          const ins = INSERTS[Math.floor(next() * INSERTS.length)];
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
