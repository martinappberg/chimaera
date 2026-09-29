import { describe, expect, it } from "vitest";

import { referenceHoverTarget, referenceMatcher, type RefSource } from "./references";

function source(patterns: string[]): RefSource {
  return {
    id: "t",
    root: "/ws",
    shapes: patterns.map((pattern) => ({ kind: "finding", pattern })),
    lookup: () => [],
  };
}

describe("referenceMatcher", () => {
  it("matches ids bounded by non-word, non-dash edges", () => {
    const m = referenceMatcher(new Map([["t", source(["F-\\d{1,4}"])]]))!;
    const hits = (s: string): string[] => [...s.matchAll(m.re)].map((x) => x[0]);
    expect(hits("see F-228, and (F-12).")).toEqual(["F-228", "F-12"]);
    expect(hits("xF-228 F-228x F-2281234 COVID-19")).toEqual([]);
  });

  it("skips shapes that aren't a plain bounded regex", () => {
    expect(referenceMatcher(new Map([["t", source(["(a)+", "["])]]))).toBeNull();
    const m = referenceMatcher(new Map([["t", source(["(x)", "D-\\d+"])]]))!;
    expect(m.groups).toHaveLength(1);
  });

  it("skips shapes that can match nothing (the scan would never advance)", () => {
    expect(referenceMatcher(new Map([["t", source(["\\d*", "F-\\d?"])]]))!.groups).toHaveLength(1);
    expect(referenceMatcher(new Map([["t", source(["x?"])]]))).toBeNull();
  });

  it("is null with no sources", () => {
    expect(referenceMatcher(new Map())).toBeNull();
  });
});

describe("referenceHoverTarget", () => {
  const open = (): void => {};

  it("previews a relative span's lines under its base", () => {
    const t = referenceHoverTarget([
      { key: "k", kind: "finding", title: "t", span: { path: "a/b.md", line: 4, end_line: 9 }, base: "/ws", note: "F-1", open },
    ]);
    expect(t?.target.kind).toBe("file");
    expect(t?.target.kind === "file" ? t.target.fragment : null).toBe("L4-L9");
    expect(t?.note).toBe("F-1");
  });

  it("shows nothing for a one-line span or without a base", () => {
    expect(
      referenceHoverTarget([{ key: "k", kind: "todo", title: "t", span: { path: "a.md", line: 3, end_line: 3 }, base: "/ws", open }]),
    ).toBeNull();
    expect(referenceHoverTarget([{ key: "k", kind: "x", title: "t", span: { path: "a.md", line: 1, end_line: 5 }, open }])).toBeNull();
  });

  it("says when an id names several entries", () => {
    const span = { path: "/abs.md", line: 1, end_line: 3 };
    const t = referenceHoverTarget([
      { key: "a", kind: "finding", title: "one", span, note: "F-2", open },
      { key: "b", kind: "finding", title: "two", span, note: "F-2", open },
    ]);
    expect(t?.note).toBe("F-2 · 2 entries use this id");
  });
});
