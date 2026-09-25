import { describe, expect, it } from "vitest";
import { extractFileRefs, FILE_REF_MAX_BYTES, findFileRef, parseFileRef, type FileRef } from "./fileRef";
import fixture from "./fileRefs.fixture.json";

interface Expected {
  path: string;
  line?: number;
  col?: number;
  endLine?: number;
}

/** The parse, as the fixture spells it (absent fields omitted). */
function shape(ref: FileRef): Expected {
  const out: Expected = { path: ref.path };
  if (ref.line !== undefined) out.line = ref.line;
  if (ref.col !== undefined) out.col = ref.col;
  if (ref.endLine !== undefined) out.endLine = ref.endLine;
  return out;
}

describe("fileRefs.fixture.json: one reference", () => {
  it("has a realistic spread", () => {
    expect(fixture.refs.length + fixture.text.length).toBeGreaterThanOrEqual(60);
    for (const style of ["claude", "codex", "gemini"]) {
      expect(fixture.refs.some((c) => c.style === style)).toBe(true);
      expect(fixture.text.some((c) => c.style === style)).toBe(true);
    }
  });
  for (const c of fixture.refs) {
    const flags = [c.delimited === true ? "delimited" : "", c.bare === true ? "bare" : ""].filter(Boolean);
    it(`${c.style}: ${JSON.stringify(c.input)}${flags.length > 0 ? ` (${flags.join(", ")})` : ""}`, () => {
      const ref = parseFileRef(c.input, { delimited: c.delimited === true, bare: c.bare === true });
      expect(ref === null ? null : shape(ref)).toEqual(c.expect);
    });
  }
});

describe("fileRefs.fixture.json: references in text", () => {
  for (const c of fixture.text) {
    it(`${c.style}: ${JSON.stringify(c.input)}`, () => {
      const found = extractFileRefs(c.input).map((f) => ({
        text: c.input.slice(f.start, f.end),
        ...shape(f.ref),
      }));
      expect(found).toEqual(c.expect);
    });
  }
});

describe("findFileRef offsets", () => {
  it("underlines the reference, not its wrappers or punctuation", () => {
    const t = '("src/x.rs:12").';
    const f = findFileRef(t);
    expect(f).not.toBeNull();
    expect(t.slice(f!.start, f!.end)).toBe("src/x.rs:12");
  });
  it("keeps grep's content out of the link", () => {
    const t = "src/x.rs:12:3:let x = 1;";
    const f = findFileRef(t.split(" ")[0]);
    expect(t.slice(f!.start, f!.end)).toBe("src/x.rs:12:3");
    expect(shape(f!.ref)).toEqual({ path: "src/x.rs", line: 12, col: 3 });
  });
  it("covers a mention's @ and a URL's scheme", () => {
    expect(findFileRef("@src/x.ts")).toMatchObject({ start: 0, end: 9 });
    const url = "file:///tmp/x.log";
    expect(findFileRef(url)).toMatchObject({ start: 0, end: url.length });
  });
});

describe("length ceiling", () => {
  it("admits a path up to the daemon's byte cap", () => {
    const long = `${"a/".repeat(509)}x.md`; // 1022 bytes
    expect(parseFileRef(long)?.path).toBe(long);
  });
  it("rejects one past it, counting UTF-8 bytes", () => {
    const seg = "é".repeat(100); // 200 bytes
    const long = `${`${seg}/`.repeat(6)}x.md`;
    expect(new TextEncoder().encode(long).length).toBeGreaterThan(FILE_REF_MAX_BYTES);
    expect(parseFileRef(long)).toBeNull();
  });
});
