import { describe, expect, it } from "vitest";
import { Marked } from "marked";

import { markdownMath } from "./math";
import { advanceSegments, BAIL_SEGMENT_CAP, type SegmenterState } from "./streamSegments";

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

/** The component's exact marked configuration (math extension + breaks), on a
 *  private instance so these tests can't perturb the global marked. */
const md = new Marked();
md.use(markdownMath);
const parse = (src: string) => md.parse(src, { async: false, breaks: true }) as string;

/** Render every closed segment + the open tail separately and join — the
 *  streaming render's HTML — for comparison against the whole-document parse
 *  (the settled render). Equal ⇒ the split was safe. */
function joinedParse(chunks: string[]): { joined: string; whole: string } {
  const { closed, open } = feed(chunks);
  const joined =
    closed.map((s) => parse(s)).join("") + (open.trim().length > 0 ? parse(open) : "");
  return { joined, whole: parse(chunks[chunks.length - 1]) };
}

describe("streamSegments: lossless partition", () => {
  const STREAMS: string[][] = [
    ["para one", "para one\n\npara t", "para one\n\npara two\n\n# heading\ntail"],
    ["```js\ncode", "```js\ncode\n\nstill code\n```\n\nafter"],
    ["- a\n- b\n\n", "- a\n- b\n\n  loose continuation\n\nflush para\n\nnext"],
    ["$$\nx = 1", "$$\nx = 1\n\ny = 2\n$$\n\nprose"],
    ["text\n\n\n\nmore blank runs\n\n", "text\n\n\n\nmore blank runs\n\nend"],
    ["crlf one\r\n\r\ncrlf", "crlf one\r\n\r\ncrlf two\r\n\r\ntail"],
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

  it("the incremental scan is chunking-invariant: any chunking yields the whole-feed split", () => {
    const full =
      "intro para\n\n# heading\n\n```\nfence\n\nstill fence\n```\n\n- item\n- item2\n\nflush close\n\n$$\nE=mc^2\n$$\n\nfinal tail";
    const wholeFeed = feed([full]);
    for (const step of [1, 3, 7, 64]) {
      const chunks: string[] = [];
      for (let i = step; i < full.length; i += step) chunks.push(full.slice(0, i));
      chunks.push(full);
      const chunked = feed(chunks);
      expect(chunked.closed).toEqual(wholeFeed.closed);
      expect(chunked.open).toBe(wholeFeed.open);
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

  it("CRLF streams close paragraphs and fences (marked normalizes \\r\\n)", () => {
    const para = feed(["para one\r\n\r\npara two"]);
    expect(para.closed).toEqual(["para one\r\n\r\n"]);
    const fence = feed(["```\r\ncode\r\n```\r\n\r\nafter"]);
    expect(fence.closed).toEqual(["```\r\ncode\r\n```\r\n\r\n"]);
  });

  it("keeps a list open across blank lines (loose continuation lookahead)", () => {
    const { closed, open } = feed(["- item one\n- item two\n\n  continuation para\n\nstill"]);
    expect(closed).toEqual([]);
    expect(open).toBe("- item one\n- item two\n\n  continuation para\n\nstill");
  });

  it("keeps a list open past a LAZY continuation line (flush plain text under an item)", () => {
    // "lazy cont" is list-item content (lazy continuation); "  indented para"
    // continues the item across the blank line. Closing after "lazy cont"
    // would render the continuation as a top-level paragraph until settle.
    const { closed } = feed(["- item\nlazy cont\n\n  indented para\n\nmore"]);
    expect(closed).toEqual([]);
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

  it("never splits inside $$ block math spanning blank lines", () => {
    const { closed, open } = feed(["$$\na = 1\n\nb = 2\n$$\n\nprose"]);
    expect(closed).toEqual(["$$\na = 1\n\nb = 2\n$$\n\n"]);
    expect(open).toBe("prose");
  });

  it("never splits inside \\[ ... \\] block math", () => {
    const { closed } = feed(["\\[\na = 1\n\nb = 2"]);
    expect(closed).toEqual([]);
  });

  it("emits nothing new when the text is unchanged", () => {
    const a = advanceSegments(null, "one\n\ntwo");
    const b = advanceSegments(a.state, "one\n\ntwo");
    expect(b.extended).toBe(true);
    expect(b.newlyClosed).toEqual([]);
    expect(b.open).toBe("two");
  });
});

describe("streamSegments: hostile raw-HTML input (the streaming render must never be MORE permissive than settle)", () => {
  it("a <pre> block interior can never close into live DOM (the beacon repro)", () => {
    // Whole-doc parse keeps the image line INERT (raw text inside <pre>).
    // A split after "plain" would parse the image line alone → a live <img>
    // fires a network beacon from agent output. The line opening the HTML
    // block must bail segmentation for the whole message.
    const hostile = "<pre>\nplain\n\n![x](https://evil.example/beacon.png)\n\n</pre>";
    const { closed, state } = feed([hostile.slice(0, 12), hostile]);
    expect(closed).toEqual([]);
    expect(state.bail).toBe("html");
  });

  it("comment-hidden content stays hidden", () => {
    const { closed, state } = feed(["<!--\nhidden\n\n[click](https://evil/phish)\n\n-->"]);
    expect(closed).toEqual([]);
    expect(state.bail).toBe("html");
  });

  it("script/style/textarea, processing instructions, declarations, and close-tag lines all bail", () => {
    for (const opener of [
      "<script>\nx\n\n![b](https://evil/1.png)",
      "<style>\n.x{}\n\ncontent",
      "<textarea>\nraw\n\ncontent",
      "<?php\nx\n\ncontent",
      "<!DOCTYPE html>\n\ncontent",
      "<![CDATA[\nraw\n\ncontent",
      "</div>\n\ncontent",
      "<div>\n\ncontent\n\nmore",
    ]) {
      const { closed, state } = feed([opener]);
      expect(state.bail).toBe("html");
      expect(closed).toEqual([]);
    }
  });

  it("an HTML bail is permanent for the message and never force-closes past the size cap", () => {
    const big = "<pre>\nstart\n\n" + "filler line\n\n".repeat(4000); // ~52 KiB
    const { closed, state } = feed([big]);
    expect(state.bail).toBe("html");
    expect(closed).toEqual([]);
  });

  it("inline HTML mid-line does not bail (only line-leading tags can open an HTML block)", () => {
    const { closed, state } = feed(["some <b>bold</b> text\n\nnext para\n\ntail"]);
    expect(state.bail).toBe(false);
    expect(closed).toEqual(["some <b>bold</b> text\n\n", "next para\n\n"]);
  });
});

describe("streamSegments: scanner pinned to math.ts (marked equivalence)", () => {
  it("a `$$ ` line with trailing whitespace is NOT a math closer (BLOCK_DOLLAR needs the exact line)", () => {
    // If the scanner accepted "$$ " as a closer it would allow a split inside
    // still-open math — prose the reader saw would collapse into a katex
    // error blob at settle.
    const { closed } = feed(["$$\nx = 1\n$$ \n\nprose that must not split\n\nmore"]);
    expect(closed).toEqual([]);
  });

  it("`\\]` mid-line does not close bracket math (the regex needs it at line end)", () => {
    const { closed } = feed(["\\[\na = 1 \\] not a closer\n\nb = 2\n\nc"]);
    expect(closed).toEqual([]);
  });

  it("splits the scanner allows render identically segmented and whole (math included)", () => {
    const STREAMS: string[][] = [
      ["$$\nE = mc^2\n$$\n\nafter math\n\ntail"],
      ["\\[\na = b\n\\]\n\nafter\n\ntail"],
      ["inline $x+y$ math\n\nnext para\n\ntail"],
      ["  $$\nnot an opener (indented)\n\nplain paragraph\n\ntail"],
      ["para\n\n$$\nx\n$$\n\npara2\n\ntail"],
    ];
    for (const chunks of STREAMS) {
      const { joined, whole } = joinedParse(chunks);
      expect(joined).toBe(whole);
    }
  });

  it("paragraph/heading/list/fence splits render identically segmented and whole", () => {
    const STREAMS: string[][] = [
      ["intro para\n\n# heading\n\n- a\n- b\n\nclosing flush para\n\ntail para"],
      ["```js\nconst x = 1;\n\nconst y = 2;\n```\n\nafter\n\ntail"],
      ["> quote line\n\nplain para\n\ntail"],
      ["| a | b |\n|---|---|\n| 1 | 2 |\n\nafter table\n\ntail"],
    ];
    for (const chunks of STREAMS) {
      const { joined, whole } = joinedParse(chunks);
      expect(joined).toBe(whole);
    }
  });
});

describe("streamSegments: reference definitions", () => {
  it("a definition makes boundaries sticky for the message", () => {
    const first = advanceSegments(null, "see [docs][1]\n\n[1]: https://example.com\n\nafter");
    // The paragraph BEFORE the definition may have closed (a usage that
    // closed before its definition streamed renders literally until settle —
    // the accepted limitation); nothing at or beyond the definition closes.
    expect(first.state.scan.refDefSeen).toBe(true);
    expect(first.open.includes("[1]: https://example.com")).toBe(true);
    const second = advanceSegments(first.state, first.state.source + "\n\nmore\n\nprose");
    expect(second.newlyClosed).toEqual([]);
    expect(second.state.scan.refDefSeen).toBe(true);
  });

  it("matches definitions with escaped brackets in the label", () => {
    const { state } = feed(["[foo\\]bar]: https://example.com\n\nafter"]);
    expect(state.scan.refDefSeen).toBe(true);
  });

  it("past the size cap, closes resume at safe boundaries only (bounded tail)", () => {
    const para = "a plain filler paragraph of prose for cap testing\n\n";
    let text = "[1]: https://example.com\n\n";
    while (text.length < BAIL_SEGMENT_CAP + 4 * para.length) text += para;
    const { closed, open, state } = feed([text.slice(0, 100), text]);
    expect(state.scan.refDefSeen).toBe(true);
    expect(closed.length).toBeGreaterThan(0);
    // Lossless and bounded: what stays open is under cap + one paragraph.
    expect(closed.join("") + open).toBe(text);
    expect(open.length).toBeLessThanOrEqual(BAIL_SEGMENT_CAP + para.length);
    // Every forced close still landed on a safe blank-line boundary.
    for (const seg of closed) expect(seg.endsWith("\n\n")).toBe(true);
  });

  it("the size cap never relaxes an open fence", () => {
    let text = "[1]: https://example.com\n\n```\n";
    while (text.length < BAIL_SEGMENT_CAP * 2) text += "inert fence line with ![x](https://evil/b.png)\n\n";
    const { closed } = feed([text]);
    expect(closed).toEqual([]); // still inside the fence — no relief, no split
  });
});

describe("streamSegments: prefix-cache invalidation", () => {
  it("a text that does not extend the previous one rebuilds from scratch", () => {
    const a = advanceSegments(null, "para one\n\npara two\n\ntail");
    expect(a.newlyClosed).toEqual(["para one\n\n", "para two\n\n"]);
    const b = advanceSegments(a.state, "rewritten\n\ntail");
    expect(b.extended).toBe(false);
    expect(b.newlyClosed).toEqual(["rewritten\n\n"]);
    expect(b.open).toBe("tail");
  });

  it("a shrunk text invalidates too", () => {
    const a = advanceSegments(null, "para one\n\npara two");
    const b = advanceSegments(a.state, "para");
    expect(b.extended).toBe(false);
    expect(b.newlyClosed).toEqual([]);
    expect(b.open).toBe("para");
  });

  it("a rewrite also resets sticky state (bail, refDefSeen)", () => {
    const a = advanceSegments(null, "<pre>\nraw");
    expect(a.state.bail).toBe("html");
    const b = advanceSegments(a.state, "plain para\n\ntail");
    expect(b.extended).toBe(false);
    expect(b.state.bail).toBe(false);
    expect(b.newlyClosed).toEqual(["plain para\n\n"]);
  });
});

describe("streamSegments: paragraph breaks at agent text-block boundaries (PR #122)", () => {
  it("the reducer's materialized \\n\\n separator is neither eaten nor doubled", () => {
    const { closed, open } = feed(["blockA", "blockA\n\n", "blockA\n\nblockB"]);
    expect(closed).toEqual(["blockA\n\n"]);
    expect(open).toBe("blockB");
    const joined = closed.map((s) => parse(s)).join("") + parse(open);
    expect(joined).toBe(parse("blockA\n\nblockB"));
  });

  it("multi-paragraph streams render identically segmented and whole", () => {
    const full = "intro para\n\n# heading\n\n- a\n- b\n\nclosing flush para\n\ntail para";
    const { joined, whole } = joinedParse([full]);
    expect(joined).toBe(whole);
  });
});
