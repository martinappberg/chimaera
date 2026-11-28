import { describe, expect, it } from "vitest";
import { FORMULA_MAX_DEPTH, FORMULA_MAX_LENGTH, evalFormula, formulaValue, parseFormula } from "./pptxFormula";

/** The arithmetic in PowerPoint's time values, and everything it refuses. */

const v = { ppt_x: 0.4, ppt_y: 0.3, ppt_w: 0.2, ppt_h: 0.1 };

describe("formulaValue", () => {
  it("reads what the corpus decks write", () => {
    expect(formulaValue("#ppt_x", v)).toBeCloseTo(0.4);
    expect(formulaValue("#ppt_y+.1", v)).toBeCloseTo(0.4);
    expect(formulaValue("1+#ppt_h/2", v)).toBeCloseTo(1.05);
    expect(formulaValue("0-#ppt_w/2", v)).toBeCloseTo(-0.1);
    expect(formulaValue("4*ppt_w", v)).toBeCloseTo(0.8);
    expect(formulaValue("#ppt_w*.05", v)).toBeCloseTo(0.01);
    expect(formulaValue("0", v)).toBe(0);
  });

  it("keeps precedence, unary signs and parentheses", () => {
    expect(formulaValue("1+2*3", v)).toBe(7);
    expect(formulaValue("(1+2)*3", v)).toBe(9);
    expect(formulaValue("-2*-3", v)).toBe(6);
    expect(formulaValue("8/2/2", v)).toBe(2);
    expect(formulaValue("1-2-3", v)).toBe(-4);
    expect(formulaValue(" ( ppt_x + ppt_y ) * 10 ", v)).toBeCloseTo(7);
    expect(formulaValue("1e1+1.5E-1", v)).toBeCloseTo(10.15);
  });

  it("refuses anything outside the grammar", () => {
    for (const bad of [
      "",
      "   ",
      "$",
      "#ppt_x+$",
      "sin(1)",
      "ppt_r",
      "#foo",
      "constructor",
      "__proto__",
      "1+",
      "(1",
      "1)",
      "1 2",
      "1**2",
      "1;2",
      "alert(1)",
      "ppt_x.y",
      "#",
      "1/0",
    ]) {
      expect(formulaValue(bad, v), bad).toBe(null);
    }
  });

  it("bounds the source and its nesting", () => {
    expect(formulaValue("1+".repeat(200) + "1", v)).toBe(null);
    expect(parseFormula("1".padEnd(FORMULA_MAX_LENGTH, " "))).not.toBe(null);
    expect(parseFormula("1".padEnd(FORMULA_MAX_LENGTH + 1, " "))).toBe(null);
    const nest = (n: number) => "(".repeat(n) + "1" + ")".repeat(n);
    expect(parseFormula(nest(FORMULA_MAX_DEPTH))).not.toBe(null);
    expect(parseFormula(nest(FORMULA_MAX_DEPTH + 1))).toBe(null);
    expect(parseFormula("-".repeat(FORMULA_MAX_DEPTH + 2) + "1")).toBe(null);
  });

  it("parses once, evaluates for any shape", () => {
    const f = parseFormula("#ppt_x+#ppt_w/2")!;
    expect(evalFormula(f, v)).toBeCloseTo(0.5);
    expect(evalFormula(f, { ...v, ppt_x: 0, ppt_w: 1 })).toBeCloseTo(0.5);
  });
});
