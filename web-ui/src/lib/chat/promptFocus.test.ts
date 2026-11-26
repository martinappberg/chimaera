import { describe, expect, it } from "vitest";
import { yieldsFocus } from "./promptFocus";

describe("yieldsFocus", () => {
  it("takes the keyboard when nothing is being typed", () => {
    expect(yieldsFocus(null)).toBe(true);
    expect(yieldsFocus({ text: "  ", inChat: true })).toBe(true);
  });

  it("never interrupts a draft, or a field in another pane", () => {
    expect(yieldsFocus({ text: "2 more things", inChat: true })).toBe(false);
    // A terminal's input is empty between keystrokes: still someone typing.
    expect(yieldsFocus({ text: "", inChat: false })).toBe(false);
  });
});
