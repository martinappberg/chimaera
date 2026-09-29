import { describe, expect, it } from "vitest";
import { Text } from "@codemirror/state";
import { toMarks } from "./editorMarks";
import type { Diagnostic } from "./platform";

const doc = Text.of(["\\documentclass{article}", "\\begin{document}", "Hello \\unit{mg}", "\\end{document}"]);
const d = (o: Partial<Diagnostic>): Diagnostic => ({ file: "a.tex", severity: "error", line: 1, message: "m", plugin: "latex", ...o });

describe("a plugin's problems as editor marks", () => {
  it("marks a whole line, or from a column to the end of the range", () => {
    const [whole, col] = toMarks(doc, [d({ line: 3 }), d({ line: 3, column: 7, end_line: 3, end_column: 16 })]);
    expect(doc.sliceString(whole.from, whole.to)).toBe("Hello \\unit{mg}");
    expect(doc.sliceString(col.from, col.to)).toBe("\\unit{mg}");
    expect(whole.source).toBe("latex");
  });

  it("keeps errors and warnings, drops info, hints and lines past the end", () => {
    const marks = toMarks(doc, [
      d({ severity: "warning", line: 2 }),
      d({ severity: "info", line: 2 }),
      d({ severity: "hint", line: 2 }),
      d({ line: 9 }),
      d({ line: 0 }),
    ]);
    expect(marks.map((m) => m.severity)).toEqual(["warning"]);
  });

  it("never reaches past a line for a column beyond it", () => {
    const [m] = toMarks(doc, [d({ line: 1, column: 500 })]);
    expect(m.from).toBe(doc.line(1).to);
    expect(m.to).toBe(doc.line(1).to);
  });
});
