import { describe, expect, it } from "vitest";
import { findNavigation } from "./find";
import { textMatches } from "./findText";

const key = (key: string, modifiers: Partial<KeyboardEvent> = {}) => ({
  key, metaKey: false, ctrlKey: false, shiftKey: false, altKey: false, isComposing: false, ...modifiers,
}) as KeyboardEvent;

describe("find shortcuts", () => {
  it("leaves bare terminal control keys alone", () => {
    expect(findNavigation(key("f", { ctrlKey: true }), true)).toBeNull();
    expect(findNavigation(key("g", { ctrlKey: true }), true)).toBeNull();
    expect(findNavigation(key("f", { ctrlKey: true }), false)).toBe("open");
  });
  it("navigates without taking unrelated or composing shortcuts", () => {
    expect(findNavigation(key("g", { metaKey: true }), true)).toBe("next");
    expect(findNavigation(key("G", { metaKey: true, shiftKey: true }), true)).toBe("previous");
    expect(findNavigation(key("F3", { shiftKey: true }), true)).toBe("previous");
    expect(findNavigation(key("g", { metaKey: true, isComposing: true }), false)).toBeNull();
    expect(findNavigation(key("g", { metaKey: true, altKey: true }), false)).toBeNull();
  });
});

describe("literal find offsets", () => {
  it("treats regular expression punctuation literally", () => {
    expect(textMatches("a.*[x] a.*[x] ax", "a.*[x]")).toEqual([[0, 6], [7, 13]]);
  });
  it("keeps source offsets after Unicode and emoji, without lowercasing the source", () => {
    expect(textMatches("İ😀 Ångström ångström", "ångström")).toEqual([[4, 12], [13, 21]]);
    expect(textMatches("İ😀 Ångström ångström", "ångström", true)).toEqual([[13, 21]]);
  });
  it("bounds dense matches and never matches an empty query", () => {
    expect(textMatches("a".repeat(2000), "a", false, 3)).toHaveLength(3);
    expect(textMatches("text", "")).toEqual([]);
    expect(textMatches("a", "a", false, 0)).toEqual([]);
  });
});
