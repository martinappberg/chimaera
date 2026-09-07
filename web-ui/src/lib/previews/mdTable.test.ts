import { markdown, markdownLanguage } from "@codemirror/lang-markdown";
import { Text } from "@codemirror/state";
import type { SyntaxNode } from "@lezer/common";
import { describe, expect, it } from "vitest";

import { mathExtension } from "./mdMath";
import { alignments, tableModel, type Inline, type TableModel } from "./mdTable";

/** The live preview's grammar: GFM plus the file `$` dialect. */
const parser = markdown({ base: markdownLanguage, extensions: [mathExtension] }).language.parser;

function tables(src: string): TableModel[] {
  const doc = Text.of(src.split("\n"));
  const out: TableModel[] = [];
  parser.parse(src).iterate({
    enter: (n) => {
      if (n.name !== "Table") return;
      const m = tableModel(n.node as SyntaxNode, doc);
      if (m !== null) out.push(m);
      return false;
    },
  });
  return out;
}

/** A cell's inline tree flattened to a compact string for assertions. */
function flat(inline: Inline[]): string {
  return inline
    .map((i) => {
      switch (i.kind) {
        case "text":
          return i.text;
        case "math":
          return `$${i.source}$`;
        case "link":
          return `[${flat(i.children)}](${i.url})`;
        default:
          return `<${i.kind}>${flat(i.children)}</${i.kind}>`;
      }
    })
    .join("");
}

describe("alignments", () => {
  it("reads the delimiter row like comrak writes align", () => {
    expect(alignments("|:--|--:|:-:|---|")).toEqual(["left", "right", "center", null]);
    expect(alignments(":-- | --:")).toEqual(["left", "right"]);
  });
});

describe("tableModel", () => {
  const src = [
    "Intro",
    "",
    "| Sample | Value | Status |",
    "|:--|--:|:-:|",
    "| GM12878 | **1.50** | ok |",
    "| K562 | `x` \\| y |  |",
    "| a | *b* c | [link](https://example.com) |",
    "| only |",
    "| x | y | z | extra |",
    "",
    "After",
  ].join("\n");

  it("models header, alignment, rows, and pads or trims cells to the header", () => {
    const [t] = tables(src);
    expect(t.align).toEqual(["left", "right", "center"]);
    expect(t.header.map((c) => flat(c.inline))).toEqual(["Sample", "Value", "Status"]);
    expect(t.rows.map((r) => r.map((c) => flat(c.inline)))).toEqual([
      ["GM12878", "<strong>1.50</strong>", "ok"],
      ["K562", "<code>x</code> | y", ""],
      ["a", "<em>b</em> c", "[link](https://example.com)"],
      ["only", "", ""],
      ["x", "y", "z"],
    ]);
    expect(t.source.startsWith("| Sample")).toBe(true);
    expect(t.source.endsWith("| extra |")).toBe(true);
  });

  it("gives every cell a source range a click can land in, empty ones included", () => {
    const [t] = tables(src);
    const header = t.header[1];
    expect(src.slice(header.from, header.to)).toBe("Value");
    const empty = t.rows[1][2]; // `|  |` — no node, just the pipes
    expect(src.slice(empty.from - 2, empty.from)).toBe("| ");
    expect(empty.from).toBe(empty.to);
    const padded = t.rows[3][1]; // a missing trailing cell sits at the row's end
    expect(src.slice(t.rows[3][0].from, padded.from)).toBe("only |");
  });

  it("renders math through the file dialect and keeps a quoted table's cells", () => {
    const [t] = tables("| e | q |\n|---|---|\n| $E=mc^2$ | plain $5 and $10 |\n");
    expect(t.rows[0].map((c) => flat(c.inline))).toEqual(["$E=mc^2$", "plain $5 and $10"]);
    expect(t.rows[0][0].inline[0]).toEqual({ kind: "math", source: "E=mc^2", display: false });

    const [q] = tables("> | q | r |\n> |---|---|\n> | 1 | 2 |\n");
    expect(q.header.map((c) => flat(c.inline))).toEqual(["q", "r"]);
    expect(q.rows).toHaveLength(1);
    expect(q.rows[0].map((c) => flat(c.inline))).toEqual(["1", "2"]);
    expect(q.source).toContain("> |---|---|");
  });
});
