import { describe, expect, it } from "vitest";
import { wordNavigationInput } from "./terminalKeys";

const left = {
  type: "keydown", key: "ArrowLeft", altKey: true,
  ctrlKey: false, metaKey: false, shiftKey: false, isComposing: false,
};

describe("macOS terminal word navigation", () => {
  it("sends the same readline word movements locally and remotely", () => {
    expect(wordNavigationInput(left, true, "normal", false)).toBe("\x1bb");
    expect(wordNavigationInput({ ...left, key: "ArrowRight" }, true, "normal", false)).toBe("\x1bf");
  });
  it("leaves full-screen and application-cursor keys to the terminal", () => {
    expect(wordNavigationInput(left, true, "alternate", false)).toBeNull();
    expect(wordNavigationInput(left, true, "normal", true)).toBeNull();
  });
  it("preserves other platforms, modifier chords, composition, and Option characters", () => {
    expect(wordNavigationInput(left, false, "normal", false)).toBeNull();
    for (const change of [
      { altKey: false }, { ctrlKey: true }, { metaKey: true }, { shiftKey: true },
      { isComposing: true }, { type: "keyup" }, { type: "keypress" },
      { key: "ArrowUp" }, { key: "ArrowDown" }, { key: "å" }, { key: "f" }, { key: "p" },
    ]) expect(wordNavigationInput({ ...left, ...change }, true, "normal", false)).toBeNull();
  });
});
