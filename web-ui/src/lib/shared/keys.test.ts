import { afterEach, describe, expect, it, vi } from "vitest";
import { chordDigit, matchChord, paneMoveChord, parseChord } from "./keys";

function digit(modifiers: Partial<KeyboardEvent>): KeyboardEvent {
  return { code: "Digit2", key: "@", metaKey: false, ctrlKey: false, altKey: false, shiftKey: false, ...modifiers } as KeyboardEvent;
}

describe("numbered pane shortcuts", () => {
  it("distinguishes the session chord from the move chord using physical digits", () => {
    const session = digit({ metaKey: true });
    const move = digit({ metaKey: true, ctrlKey: true });
    expect(chordDigit(session, "cmd")).toBe(2);
    expect(chordDigit(session, "cmd", true)).toBeNull();
    expect(chordDigit(move, "cmd")).toBeNull();
    expect(chordDigit(move, "cmd", true)).toBe(2);
  });

  it("uses Control+Cmd for every destination while leaving screenshot chords alone", () => {
    for (let n = 1; n <= 9; n++) {
      const move = digit({ metaKey: true, ctrlKey: true, code: `Digit${n}` });
      expect(chordDigit(move, "cmd", true)).toBe(n);
      expect(matchChord(move, parseChord(paneMoveChord(n, "cmd"), "cmd")!)).toBe("hit");
      expect(chordDigit(digit({ metaKey: true, shiftKey: true, code: `Digit${n}` }), "cmd", true)).toBeNull();
    }
  });

  it("uses Alt for the second layer when the base already spends Shift", () => {
    expect(chordDigit(digit({ ctrlKey: true, shiftKey: true }), "ctrl-shift")).toBe(2);
    expect(chordDigit(digit({ ctrlKey: true, shiftKey: true, altKey: true }), "ctrl-shift", true)).toBe(2);
    expect(chordDigit(digit({ ctrlKey: true, shiftKey: true }), "ctrl-shift", true)).toBeNull();
    expect(chordDigit(digit({ altKey: true, shiftKey: true }), "alt", true)).toBe(2);
  });

  it("rejects unrelated modifiers and non-digit keys", () => {
    expect(chordDigit(digit({ metaKey: true, altKey: true, shiftKey: true }), "cmd", true)).toBeNull();
    expect(chordDigit(digit({ metaKey: true, ctrlKey: true, code: "KeyQ" }), "cmd", true)).toBeNull();
  });
});

describe("host-specific pane-tab defaults", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.resetModules();
  });

  it.each(["MacIntel", "Win32", "Linux x86_64"])("uses browser-deliverable chords on %s", async (platform) => {
    vi.stubGlobal("window", {});
    vi.stubGlobal("navigator", { platform });
    vi.resetModules();
    const { ACTION_BY_ID } = await import("./keys");
    expect(ACTION_BY_ID.get("cyclePrev")?.def).toBe("Mod+Alt+[");
    expect(ACTION_BY_ID.get("cycleNext")?.def).toBe("Mod+Alt+]");
  });

  it.each(["MacIntel", "Win32", "Linux x86_64"])("keeps native editor tab cycling on %s", async (platform) => {
    vi.stubGlobal("window", { __TAURI__: {} });
    vi.stubGlobal("navigator", { platform });
    vi.resetModules();
    const { ACTION_BY_ID } = await import("./keys");
    expect(ACTION_BY_ID.get("cyclePrev")?.def).toBe("Ctrl+Shift+Tab");
    expect(ACTION_BY_ID.get("cycleNext")?.def).toBe("Ctrl+Tab");
  });
});
