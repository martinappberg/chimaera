import { describe, expect, it } from "vitest";
import { nextVersionKey, VERSION_KEY_START, type VersionKey } from "./versionKey";

function walk(steps: [string, string | null][]): number[] {
  let k: VersionKey = VERSION_KEY_START;
  return steps.map(([path, mtime]) => (k = nextVersionKey(k, path, mtime)).key);
}

describe("nextVersionKey", () => {
  it("keeps the key when the first token arrives (a cold open)", () => {
    expect(walk([["/w/book.xlsx", null], ["/w/book.xlsx", "v1"], ["/w/book.xlsx", "v1"]])).toEqual([0, 0, 0]);
  });

  it("moves the key when the file changes on disk", () => {
    expect(walk([["/w/book.xlsx", null], ["/w/book.xlsx", "v1"], ["/w/book.xlsx", "v2"], ["/w/book.xlsx", "v3"]])).toEqual(
      [0, 0, 1, 2],
    );
  });

  it("remembers the last token across a gap", () => {
    // Vanished (no token), then back unchanged: no remount; back changed: one.
    expect(walk([["/w/a.pdf", "v1"], ["/w/a.pdf", null], ["/w/a.pdf", "v1"], ["/w/a.pdf", null], ["/w/a.pdf", "v2"]])).toEqual(
      [0, 0, 0, 0, 1],
    );
  });

  it("starts over for another path", () => {
    expect(walk([["/w/a.pdf", "v1"], ["/w/a.pdf", "v2"], ["/w/b.pdf", null], ["/w/b.pdf", "v9"], ["/w/b.pdf", "v10"]])).toEqual(
      [0, 1, 1, 1, 2],
    );
    // A path switch that arrives with its token already known is no change.
    expect(walk([["/w/a.pdf", "v1"], ["/w/b.pdf", "v1"], ["/w/b.pdf", "v1"]])).toEqual([0, 0, 0]);
  });
});
