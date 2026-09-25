import { markdown, markdownLanguage } from "@codemirror/lang-markdown";
import { Text } from "@codemirror/state";
import { describe, expect, it } from "vitest";

import { mathExtension } from "./mdMath";
import { alignments, completeRow, tableModel, type Inline, type TableModel } from "./mdTable";

/** The live preview's grammar: GFM plus the file `$` dialect. */
const parser = markdown({ base: markdownLanguage, extensions: [mathExtension] }).language.parser;

function tables(src: string): TableModel[] {
  const doc = Text.of(src.split("\n"));
  const out: TableModel[] = [];
  parser.parse(src).iterate({
    enter: (n) => {
      if (n.name !== "Table") return;
      const m = tableModel(n.node, doc);
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
        case "entity":
          return i.source;
        case "math":
          return `$${i.source}$`;
        case "link":
          return `[${flat(i.children)}](${i.url})`;
        case "image":
          return `![${i.alt}](${i.url})`;
        default:
          return `<${i.kind}>${flat(i.children)}</${i.kind}>`;
      }
    })
    .join("");
}

const cellsOf = (t: TableModel): string[][] => [
  t.header.cells.map((c) => flat(c.inline)),
  ...t.rows.map((r) => r.cells.map((c) => flat(c.inline))),
];

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
    expect(cellsOf(t)).toEqual([
      ["Sample", "Value", "Status"],
      ["GM12878", "<strong>1.50</strong>", "ok"],
      ["K562", "<code>x</code> | y", ""],
      ["a", "<em>b</em> c", "[link](https://example.com)"],
      ["only", "", ""],
      ["x", "y", "z"],
    ]);
    expect(t.source.startsWith("| Sample")).toBe(true);
    expect(t.source.endsWith("| extra |")).toBe(true);
  });

  it("gives every cell a source position a click can land in, empty ones included", () => {
    const [t] = tables(src);
    const header = t.header.cells[1];
    expect(src.slice(header.from, header.from + 5)).toBe("Value");
    const empty = t.rows[1].cells[2]; // `|  |` — no node, just the pipes
    expect(src.slice(empty.from - 2, empty.from)).toBe("| ");
    const padded = t.rows[3].cells[1]; // a missing trailing cell sits at the row's end
    expect(src.slice(t.rows[3].cells[0].from, padded.from)).toBe("only |");
  });

  it("counts the columns a row really has, whichever pipes it carries", () => {
    const [t] = tables("| A | B | C | D |\n|---|---|---|---|\n| only |\nonly\na | b\n| a | b |  |\n| a | b |   \n");
    expect(t.rows.map((r) => [r.present, r.closed])).toEqual([
      [1, true],
      [1, false],
      [2, false],
      [3, true],
      [2, true],
    ]);
    expect(t.rows.every((r) => r.cells.length === 4)).toBe(true);
  });

  it("renders the cell kinds the widget has elements for, and comrak's pipe unescape", () => {
    const [t] = tables(
      [
        "| e | q |",
        "|---|---|",
        "| $E=mc^2$ | plain $5 and $10 |",
        "| `a\\|b` | $a \\| b$ |",
        "| &amp; &#x27; <!-- c --> x | <b>raw</b> |",
        "| ![alt](fig.png) | https://example.com www.example.org <https://x.dev> |",
        "| [sp](<https://e.com/x y>) | ![i](<a b.png>) |",
      ].join("\n"),
    );
    expect(cellsOf(t).slice(1)).toEqual([
      ["$E=mc^2$", "plain $5 and $10"],
      ["<code>a|b</code>", "$a | b$"],
      ["&amp; &#x27;  x", "<b>raw</b>"],
      [
        "![alt](fig.png)",
        "[https://example.com](https://example.com) [www.example.org](www.example.org) [https://x.dev](https://x.dev)",
      ],
      ["[sp](https://e.com/x y)", "![i](a b.png)"],
    ]);
    expect(t.rows[0].cells[0].inline[0]).toEqual({ kind: "math", source: "E=mc^2", display: false });
    expect(t.rows[2].cells[0].inline[0]).toEqual({ kind: "entity", source: "&amp;" });
    expect(t.rows[3].cells[0].inline[0]).toMatchObject({ kind: "image", alt: "alt", url: "fig.png" });
  });

  it("keeps a quoted table's cells", () => {
    const [q] = tables("> | q | r |\n> |---|---|\n> | 1 | 2 |\n");
    expect(cellsOf(q)).toEqual([
      ["q", "r"],
      ["1", "2"],
    ]);
    expect(q.source).toContain("> |---|---|");
  });
});

describe("completeRow", () => {
  const header = "| A | B | C | D |\n|---|---|---|---|\n";

  /** Apply the completion for a press on `col`, type `x` there, and read
   *  back which column the parser puts it in. */
  function typedInto(row: string, col: number): { text: string; cells: string[] } {
    const src = header + row + "\n";
    const [t] = tables(src);
    const fill = completeRow(t.rows[0], 4, col);
    const anchor = fill === null ? t.rows[0].cells[col].from : fill.anchor;
    const done = fill === null ? src : src.slice(0, fill.from) + fill.insert + src.slice(fill.to);
    const typed = done.slice(0, anchor) + "x" + done.slice(anchor);
    const [after] = tables(typed);
    return { text: typed.slice(header.length).trimEnd(), cells: cellsOf(after)[1] };
  }

  it("is a no-op for a column the row already has", () => {
    const [t] = tables(header + "| a | b | c | d |\n");
    expect(completeRow(t.rows[0], 4, 3)).toBeNull();
    expect(typedInto("| a | b |  | d |", 2)).toEqual({
      text: "| a | b | x | d |",
      cells: ["a", "b", "x", "d"],
    });
  });

  it("writes the missing pipes so typing lands in the pressed column", () => {
    expect(typedInto("| only |", 1)).toEqual({
      text: "| only | x |  |  |",
      cells: ["only", "x", "", ""],
    });
    expect(typedInto("| only |", 3)).toEqual({
      text: "| only |  |  | x |",
      cells: ["only", "", "", "x"],
    });
    expect(typedInto("only", 2)).toEqual({ text: "only |  | x |  |", cells: ["only", "", "x", ""] });
    expect(typedInto("a | b", 2)).toEqual({ text: "a | b | x |  |", cells: ["a", "b", "x", ""] });
    expect(typedInto("| a | b |  |", 3)).toEqual({
      text: "| a | b |  | x |",
      cells: ["a", "b", "", "x"],
    });
    expect(typedInto("| a | b |   ", 3)).toEqual({
      text: "| a | b |  | x |",
      cells: ["a", "b", "", "x"],
    });
  });
});
