import { describe, expect, it } from "vitest";
import { indentColumns } from "./wrap";

describe("indentColumns", () => {
  it("counts leading spaces", () => {
    expect(indentColumns("        self.play(x)", 4)).toBe(8);
    expect(indentColumns("x = 1", 4)).toBe(0);
  });

  it("runs a tab to the next stop", () => {
    expect(indentColumns("\tfoo", 4)).toBe(4);
    expect(indentColumns("  \tfoo", 4)).toBe(4);
    expect(indentColumns("\t\tfoo", 8)).toBe(16);
    expect(indentColumns(" \t foo", 2)).toBe(3);
  });

  it("hangs nothing on a blank or whitespace-only line", () => {
    expect(indentColumns("", 4)).toBe(0);
    expect(indentColumns("      ", 4)).toBe(0);
    expect(indentColumns("\t\t", 4)).toBe(0);
  });
});
