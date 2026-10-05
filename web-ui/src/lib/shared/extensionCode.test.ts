import { describe, expect, it } from "vitest";
import { boundedCode, EXTENSION_CODE_CHARS, EXTENSION_CODE_LINES, sourceCodeRows, unifiedCodeRows } from "./extensionCode";

describe("extension code", () => {
  it("keeps source literal and numbers only from a valid explicit first line", () => {
    expect(sourceCodeRows("<script>\n\tfoo", 12)).toEqual([
      { kind: "source", text: "<script>", newLine: 12 },
      { kind: "source", text: "\tfoo", newLine: 13 },
    ]);
    for (const start of [undefined, 0, -2, NaN, 1.5, Infinity]) expect(sourceCodeRows("a", start)[0].newLine).toBeUndefined();
  });
  it("honors character and row ceilings without silently cutting text", () => {
    expect(boundedCode("a".repeat(EXTENSION_CODE_CHARS + 1))).toEqual({ source: "a".repeat(EXTENSION_CODE_CHARS), truncated: true });
    const bounded = boundedCode("a\n".repeat(EXTENSION_CODE_LINES + 1));
    expect(bounded.source.split("\n")).toHaveLength(EXTENSION_CODE_LINES);
    expect(bounded.truncated).toBe(true);
    expect(boundedCode("a\n")).toEqual({ source: "a\n", truncated: false });
  });
  it("maps old and new gutters across removals, additions and multiple hunks", () => {
    expect(unifiedCodeRows("--- a/x\n+++ b/x\n@@ -2,2 +2,3 @@\n same\n-old\n+new\n+extra\n@@ -9 +10 @@ label\n-last\n+final\n")).toEqual([
      { kind: "context", text: "same", oldLine: 2, newLine: 2 },
      { kind: "remove", text: "old", oldLine: 3 },
      { kind: "add", text: "new", newLine: 3 },
      { kind: "add", text: "extra", newLine: 4 },
      { kind: "hunk", text: "@@ -9 +10 @@ label" },
      { kind: "remove", text: "last", oldLine: 9 },
      { kind: "add", text: "final", newLine: 10 },
    ]);
  });
  it("handles file creation, deletion, CRLF and missing final newlines", () => {
    expect(unifiedCodeRows("@@ -0,0 +1 @@\r\n+new\r\n\\ No newline at end of file")?.[0]).toEqual({ text: "new", kind: "add", newLine: 1 });
    expect(unifiedCodeRows("@@ -1 +0,0 @@\n-old")?.[0]).toEqual({ text: "old", kind: "remove", oldLine: 1 });
  });
  it("falls back for truncated, malformed, overlong or absent hunk declarations", () => {
    for (const source of ["plain", "--- a/x\n+++ b/x", "@@ -1,2 +1,2 @@\n a", "@@ -1 +1 @@\n a\n+extra", "@@ -1 +1 @@\n?bad", "@@ -1,2 +1,2 @@\n a\n@@ -4 +4 @@\n b", "@@ -99999999999999999999999 +1 @@\n-old\n+new"]) expect(unifiedCodeRows(source)).toBeNull();
  });
  it("does not mistake header-like removed/added content for file metadata", () => {
    expect(unifiedCodeRows("@@ -1 +1 @@\n--- old text\n+++ new text")).toEqual([
      { kind: "remove", text: "-- old text", oldLine: 1 },
      { kind: "add", text: "++ new text", newLine: 1 },
    ]);
  });
  it("keeps multi-file patches literal so explicit file identities are not discarded", () => {
    const first = "--- a/first.ts\n+++ b/first.ts\n@@ -1 +1 @@\n-old\n+new\n";
    const second = "--- a/second.ts\n+++ b/second.ts\n@@ -1 +1 @@\n-before\n+after\n";
    expect(unifiedCodeRows(first + second)).toBeNull();
    expect(unifiedCodeRows("diff --git a/first.ts b/first.ts\n" + first + "diff --git a/second.ts b/second.ts\n" + second)).toBeNull();
    expect(unifiedCodeRows(first)).not.toBeNull();
  });
});
