import { describe, expect, it } from "vitest";
import { argRows } from "./toolArgs";

describe("argRows", () => {
  it("lists what the row does not already show", () => {
    expect(
      argRows({ command: "ls", description: "List", timeout: 5000, run_in_background: true }),
    ).toEqual([
      { key: "timeout", value: "5000" },
      { key: "run_in_background", value: "true" },
    ]);
  });

  it("drops an argument that repeats a location, empties and private keys", () => {
    expect(argRows({ file_path: "/w/a.rs", limit: 40, offset: null, _toolName: "x", opts: {} }, ["/w/a.rs"])).toEqual([
      { key: "limit", value: "40" },
    ]);
  });

  it("clips a long value and bounds the list", () => {
    const rows = argRows(Object.fromEntries(Array.from({ length: 20 }, (_, i) => [`k${i}`, "v".repeat(900)])));
    expect(rows).toHaveLength(12);
    expect(rows[0].value).toHaveLength(600);
  });

  it("reads nothing from a non-object", () => {
    expect(argRows("ls")).toEqual([]);
    expect(argRows(null)).toEqual([]);
  });
});
