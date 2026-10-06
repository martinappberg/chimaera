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

function letter(code: string, modifiers: Partial<KeyboardEvent>): KeyboardEvent {
  return { code, key: code.slice(3).toLowerCase(), metaKey: false, ctrlKey: false, altKey: false, shiftKey: false, ...modifiers } as KeyboardEvent;
}

describe("agent and terminal cycle chords", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.resetModules();
  });

  async function chordsOn(platform: string) {
    vi.stubGlobal("window", {});
    vi.stubGlobal("navigator", { platform });
    vi.resetModules();
    const keys = await import("./keys");
    const chord = (id: string) => keys.parseChord(keys.ACTION_BY_ID.get(id)!.def, "auto")!;
    return { keys, agents: chord("cycleAgents"), terminals: chord("cycleTerminals") };
  }

  it("is Control+Cmd+A and Control+Cmd+T on macOS, and nothing wider", async () => {
    const { keys, agents, terminals } = await chordsOn("MacIntel");
    const ctrlCmd = { metaKey: true, ctrlKey: true };
    expect(keys.matchChord(letter("KeyA", ctrlCmd), agents)).toBe("hit");
    expect(keys.matchChord(letter("KeyT", ctrlCmd), terminals)).toBe("hit");
    // ⌘T is New Terminal (the app menu), ⌘A is Select All, ⌃A is the shell's line start.
    expect(keys.matchChord(letter("KeyT", { metaKey: true }), terminals)).toBeNull();
    expect(keys.matchChord(letter("KeyA", { metaKey: true }), agents)).toBeNull();
    expect(keys.matchChord(letter("KeyA", { ctrlKey: true }), agents)).toBeNull();
    expect(keys.matchChord(letter("KeyA", { ...ctrlCmd, shiftKey: true }), agents)).toBeNull();
    expect(keys.matchChord(letter("KeyT", ctrlCmd), agents)).toBeNull();
    expect(keys.displayChord("Mod+Ctrl+a", "auto")).toBe("⌃⌘A");
  });

  it.each(["Win32", "Linux x86_64"])("adds Alt to Ctrl+Shift off macOS (%s)", async (platform) => {
    const { keys, agents, terminals } = await chordsOn(platform);
    const layer = { ctrlKey: true, shiftKey: true, altKey: true };
    expect(keys.matchChord(letter("KeyA", layer), agents)).toBe("hit");
    expect(keys.matchChord(letter("KeyT", layer), terminals)).toBe("hit");
    // Ctrl+Shift+T is the browser's reopen-tab; the second layer keeps clear of it.
    expect(keys.matchChord(letter("KeyT", { ctrlKey: true, shiftKey: true }), terminals)).toBeNull();
  });

  it("collides with no other default chord", async () => {
    for (const platform of ["MacIntel", "Win32"]) {
      const { keys } = await chordsOn(platform);
      const seen = new Map<string, string>();
      for (const action of keys.ACTIONS) {
        if (action.def === "") continue;
        const p = keys.parseChord(action.def, "auto")!;
        const sig = `${p.meta}/${p.ctrl}/${p.alt}/${p.shift}/${p.key}`;
        expect(seen.get(sig), `${action.id} shares ${action.def} with ${seen.get(sig)}`).toBeUndefined();
        seen.set(sig, action.id);
      }
    }
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
