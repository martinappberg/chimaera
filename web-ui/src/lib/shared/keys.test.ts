import { describe, expect, it } from "vitest";
import { chordDigit } from "./keys";

function digit(modifiers: Partial<KeyboardEvent>): KeyboardEvent {
  return { code: "Digit2", key: "@", metaKey: false, ctrlKey: false, altKey: false, shiftKey: false, ...modifiers } as KeyboardEvent;
}

describe("numbered pane shortcuts", () => {
  it("distinguishes the session chord from the move chord using physical digits", () => {
    const session = digit({ metaKey: true });
    const move = digit({ metaKey: true, shiftKey: true });
    expect(chordDigit(session, "cmd")).toBe(2);
    expect(chordDigit(session, "cmd", true)).toBeNull();
    expect(chordDigit(move, "cmd")).toBeNull();
    expect(chordDigit(move, "cmd", true)).toBe(2);
  });

  it("uses Alt for the second layer when the base already spends Shift", () => {
    expect(chordDigit(digit({ ctrlKey: true, shiftKey: true }), "ctrl-shift")).toBe(2);
    expect(chordDigit(digit({ ctrlKey: true, shiftKey: true, altKey: true }), "ctrl-shift", true)).toBe(2);
    expect(chordDigit(digit({ ctrlKey: true, shiftKey: true }), "ctrl-shift", true)).toBeNull();
    expect(chordDigit(digit({ altKey: true, shiftKey: true }), "alt", true)).toBe(2);
  });

  it("rejects unrelated modifiers and non-digit keys", () => {
    expect(chordDigit(digit({ metaKey: true, altKey: true, shiftKey: true }), "cmd", true)).toBeNull();
    expect(chordDigit(digit({ metaKey: true, shiftKey: true, code: "KeyQ" }), "cmd", true)).toBeNull();
  });
});
