import { describe, expect, it } from "vitest";
import { composerInputKey, composerKey, decorationRuns, fillComposerDraft } from "./nativeComposer";

describe("native composer fills", () => {
  it("positions appended decorations after the existing draft in UTF-16 units", () => {
    const next = fillComposerDraft("Plan 👩🏽‍💻: ", "ready", "append", 0, 0, [{ start: 0, end: 5, bold: true }])!;
    expect(next.text).toBe("Plan 👩🏽‍💻: ready");
    expect(next.cursor).toBe(next.text.length);
    expect(decorationRuns(next.text, next.decorations)).toEqual([
      { text: "Plan 👩🏽‍💻: ", props: {} },
      { text: "ready", props: { bold: true } },
    ]);
  });

  it("replaces a selection and leaves surrounding text undecorated", () => {
    const next = fillComposerDraft("before OLD after", "new", "insert", 7, 10, [{ start: 0, end: 3, color: "accent" }])!;
    expect(next.text).toBe("before new after");
    expect(next.cursor).toBe(10);
    expect(decorationRuns(next.text, next.decorations)).toEqual([
      { text: "before ", props: {} },
      { text: "new", props: { color: "accent" } },
      { text: " after", props: {} },
    ]);
  });

  it("keeps replace runs relative to the replacement and bounds malformed ranges", () => {
    const next = fillComposerDraft("discard me", "yes", "replace", 5, 5, [
      { start: -4, end: 100, italic: true },
      { start: 3, end: 3, bold: true },
      { start: "0", end: 2 },
    ])!;
    expect(next).toEqual({ text: "yes", cursor: 3, decorations: [{ start: 0, end: 3, italic: true }] });
    expect(fillComposerDraft("old", "new", "replace", 0, 0)).toEqual({ text: "new", cursor: 3, decorations: [] });
  });

  it("rejects oversized results and unknown placement without truncating a draft", () => {
    expect(fillComposerDraft("a".repeat(64_000), "b", "append", 0, 0)).toBeNull();
    expect(fillComposerDraft("a", "b", "prepend", 0, 0)).toBeNull();
    expect(fillComposerDraft("a".repeat(64_000), "b", "replace", 0, 0)?.text).toBe("b");
  });
});

describe("native composer edit keys", () => {
  const keyboard = { ctrlKey: false, shiftKey: false, metaKey: false, altKey: false };
  it("reports the native spelling and active modifiers for a single typed edit", () => {
    const key = composerKey({ ...keyboard, key: " ", shiftKey: true });
    expect(composerInputKey(key, { inputType: "insertText" }, false)).toEqual({ key: "space", shift: true });
    expect(composerInputKey(composerKey({ ...keyboard, key: "Backspace", altKey: true }), { inputType: "deleteWordBackward" }, false)).toEqual({ key: "backspace", meta: true });
    expect(composerKey({ ...keyboard, key: "é" })).toEqual({ key: "é" });
  });

  it("does not attribute paste, undo, IME or coalesced text to a key", () => {
    const key = composerKey({ ...keyboard, key: "v", metaKey: true });
    for (const inputType of ["insertFromPaste", "insertFromDrop", "historyUndo", "insertCompositionText", "insertFromComposition"]) expect(composerInputKey(key, { inputType }, false)).toBeUndefined();
    expect(composerInputKey(key, { inputType: "insertText" }, true)).toBeUndefined();
    expect(composerInputKey(key, { inputType: "insertText", isComposing: true }, false)).toBeUndefined();
    expect(composerKey({ ...keyboard, key: "Enter", keyCode: 229 })).toBeUndefined();
    expect(composerKey({ ...keyboard, key: "Dead" })).toBeUndefined();
  });
});
