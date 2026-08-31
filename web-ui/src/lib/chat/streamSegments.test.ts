import { describe, expect, it } from "vitest";
import { marked } from "marked";

import { advanceSegments, type SegmenterState } from "./streamSegments";

/** Feed `chunks` cumulatively (each entry is the FULL text so far, the way the
 *  reducer's growing block text reaches the renderer) and return the final
 *  advance plus every closed segment across the whole stream. */
function feed(chunks: string[]) {
  let state: SegmenterState | null = null;
  const closed: string[] = [];
  let open = "";
  let extended = true;
  for (const text of chunks) {
    const adv = advanceSegments(state, text);
    if (!adv.extended) {
      closed.length = 0;
      extended = false;
    }
    closed.push(...adv.newlyClosed);
    open = adv.open;
    state = adv.state;
  }
  return { closed, open, state: state!, extended };
}

describe("streamSegments: lossless partition", () => {
  const STREAMS: string[][] = [
    ["para one", "para one\n\npara t", "para one\n\npara two\n\n# heading\ntail"],
    ["```js\ncode", "```js\ncode\n\nstill code\n```\n\nafter"],
    ["- a\n- b\n\n", "- a\n- b\n\n  loose continuation\n\nflush para\n\nnext"],
    ["$$\nx = 1", "$$\nx = 1\n\ny = 2\n$$\n\nprose"],
    ["text\n\n\n\nmore blank runs\n\n", "text\n\n\n\nmore blank runs\n\nend"],
  ];
  it("closed segments + open tail always concatenate back to the input", () => {
    for (const chunks of STREAMS) {
      let state: SegmenterState | null = null;
      const closed: string[] = [];
      for (const text of chunks) {
        const adv = advanceSegments(state, text);
        closed.push(...adv.newlyClosed);
        state = adv.state;
        expect(closed.join("") + adv.open).toBe(text);
      }
    }
  });
});

describe("streamSegments: boundary detection", () => {
  it("closes plain paragraphs at top-level blank lines", () => {
    const { closed, open } = feed(["para one\n\npara two\n\npara three still open"]);
    expect(closed).toEqual(["para one\n\n", "para two\n\n"]);
    expect(open).toBe("para three still open");
  });

  it("treats a trailing complete blank run as a boundary", () => {
    const { closed, open } = feed(["para one\n\n"]);
    expect(closed).toEqual(["para one\n\n"]);
    expect(open).toBe("");
  });

  it("does not treat a lone trailing newline as a boundary", () => {
    const { closed, open } = feed(["para one\n"]);
    expect(closed).toEqual([]);
    expect(open).toBe("para one\n");
  });

  it("never splits inside an open fenced code block", () => {
    const { closed, open } = feed(["```\ncode\n\nmore code after a blank line"]);
    expect(closed).toEqual([]);
    expect(open).toBe("```\ncode\n\nmore code after a blank line");
  });

  it("closes a fence once its closing marker lands", () => {
    const { closed, open } = feed(["```\ncode\n\nmore\n```\n\nafter the fence"]);
    expect(closed).toEqual(["```\ncode\n\nmore\n```\n\n"]);
    expect(open).toBe("after the fence");
  });

  it("requires the closing fence to match char and length", () => {
    const { closed } = feed(["````\n```\ninner\n\nstill inside\n````\n\nout"]);
    expect(closed).toEqual(["````\n```\ninner\n\nstill inside\n````\n\n"]);
  });

  it("tilde fences work too", () => {
    const { closed, open } = feed(["~~~\nx\n~~~\n\nafter"]);
    expect(closed).toEqual(["~~~\nx\n~~~\n\n"]);
    expect(open).toBe("after");
  });

  it("keeps a list open across blank lines (loose continuation lookahead)", () => {
    const { closed, open } = feed(["- item one\n- item two\n\n  continuation para\n\nstill"]);
    // Neither boundary may close: the segment tail is a list item, then an
    // indented continuation — both continuable across blank lines.
    expect(closed).toEqual([]);
    expect(open).toBe("- item one\n- item two\n\n  continuation para\n\nstill");
  });

  it("closes list + following flush paragraph together at the next safe boundary", () => {
    const { closed, open } = feed(["- item\n\nflush paragraph\n\nnext para"]);
    expect(closed).toEqual(["- item\n\nflush paragraph\n\n"]);
    expect(open).toBe("next para");
  });

  it("ordered list items refuse the boundary too", () => {
    const { closed } = feed(["1. first\n2. second\n\nmore"]);
    expect(closed).toEqual([]);
  });

  it("indented (code-block) tails refuse the boundary", () => {
    const { closed } = feed(["    indented code\n\n    maybe same block\n\nx"]);
    expect(closed).toEqual([]);
  });

  it("HTML-ish block tails refuse the boundary", () => {
    const { closed } = feed(["<div>\n\ncontent\n\nmore <span>prose</span>"]);
    // The boundary right after "<div>" refuses (a raw-HTML block may span
    // blank lines); a later plain-prose tail line closes normally.
    expect(closed).toEqual(["<div>\n\ncontent\n\n"]);
  });

  it("never splits inside $$ block math spanning blank lines", () => {
    const { closed, open } = feed(["$$\na = 1\n\nb = 2\n$$\n\nprose"]);
    expect(closed).toEqual(["$$\na = 1\n\nb = 2\n$$\n\n"]);
    expect(open).toBe("prose");
  });

  it("never splits inside \\[ ... \\] block math", () => {
    const { closed } = feed(["\\[\na = 1\n\nb = 2"]);
    expect(closed).toEqual([]);
  });

  it("a reference-link definition bails segmentation for the message", () => {
    const first = advanceSegments(null, "see [docs][1]\n\n[1]: https://example.com\n\nafter");
    // The paragraph before the definition may have closed, but nothing at or
    // beyond the definition ever does — its effect is document-global.
    expect(first.state.bail).toBe(true);
    expect(first.open.includes("[1]: https://example.com")).toBe(true);
    const second = advanceSegments(first.state, first.state.source + "\n\nmore\n\nprose");
    expect(second.newlyClosed).toEqual([]);
    expect(second.state.bail).toBe(true);
  });

  it("emits nothing new when the text is unchanged", () => {
    const a = advanceSegments(null, "one\n\ntwo");
    const b = advanceSegments(a.state, "one\n\ntwo");
    expect(b.extended).toBe(true);
    expect(b.newlyClosed).toEqual([]);
    expect(b.open).toBe("two");
  });
});

describe("streamSegments: prefix-cache invalidation", () => {
  it("a text that does not extend the previous one rebuilds from scratch", () => {
    const a = advanceSegments(null, "para one\n\npara two\n\ntail");
    expect(a.newlyClosed).toEqual(["para one\n\n", "para two\n\n"]);
    // A retraction rewrites earlier content (messages_superseded / model
    // reroute): the advance must flag it and return the FULL fresh split.
    const b = advanceSegments(a.state, "rewritten\n\ntail");
    expect(b.extended).toBe(false);
    expect(b.newlyClosed).toEqual(["rewritten\n\n"]);
    expect(b.open).toBe("tail");
    expect(b.state.closedCount).toBe(1);
  });

  it("a shrunk text invalidates too", () => {
    const a = advanceSegments(null, "para one\n\npara two");
    const b = advanceSegments(a.state, "para");
    expect(b.extended).toBe(false);
    expect(b.newlyClosed).toEqual([]);
    expect(b.open).toBe("para");
  });
});

describe("streamSegments: paragraph breaks at agent text-block boundaries (PR #122)", () => {
  it("the reducer's materialized \\n\\n separator is neither eaten nor doubled", () => {
    // The chat reducer appends "blockA" then "\n\nblockB" into one message
    // (the coalescer materializes the separator at agent block boundaries).
    const { closed, open } = feed(["blockA", "blockA\n\n", "blockA\n\nblockB"]);
    expect(closed).toEqual(["blockA\n\n"]);
    expect(open).toBe("blockB");
    // Per-segment parses concatenate to the exact full-document parse: two
    // paragraphs, one boundary — no merged paragraph, no extra blank one.
    const opts = { async: false, breaks: true } as const;
    const joined =
      closed.map((s) => marked.parse(s, opts) as string).join("") +
      (marked.parse(open, opts) as string);
    expect(joined).toBe(marked.parse("blockA\n\nblockB", opts) as string);
  });

  it("multi-paragraph streams render identically segmented and whole", () => {
    const full = "intro para\n\n# heading\n\n- a\n- b\n\nclosing flush para\n\ntail para";
    const { closed, open } = feed([full]);
    const opts = { async: false, breaks: true } as const;
    const joined =
      closed.map((s) => marked.parse(s, opts) as string).join("") +
      (marked.parse(open, opts) as string);
    expect(joined).toBe(marked.parse(full, opts) as string);
  });
});
