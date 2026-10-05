import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { hintsActive, initChordHints, paneHintsActive } from "./chordHints.svelte";
import type { ModifierSetting } from "./keys";

const preferences = vi.hoisted(() => ({ modifier: "cmd" as ModifierSetting }));
vi.mock("./keybindings", () => ({ modifierSetting: () => preferences.modifier }));

describe("held modifier discovery", () => {
  let keys: EventTarget;
  let visibility: EventTarget;
  let stop: () => void;

  function press(key: string, flags: Partial<KeyboardEvent> = {}): void {
    keys.dispatchEvent(Object.assign(new Event("keydown"), {
      key, metaKey: false, ctrlKey: false, altKey: false, shiftKey: false, ...flags,
    }));
  }

  beforeEach(() => {
    vi.useFakeTimers();
    preferences.modifier = "cmd";
    keys = new EventTarget();
    visibility = new EventTarget();
    vi.stubGlobal("window", keys);
    vi.stubGlobal("document", visibility);
    stop = initChordHints();
  });

  afterEach(() => {
    stop();
    vi.unstubAllGlobals();
    vi.useRealTimers();
  });

  it("waits for a deliberate hold and clears as soon as a shortcut commits", () => {
    press("Meta", { metaKey: true });
    vi.advanceTimersByTime(379);
    expect(hintsActive()).toBe(false);
    vi.advanceTimersByTime(1);
    expect(hintsActive()).toBe(true);
    press("2", { metaKey: true });
    expect(hintsActive()).toBe(false);
    expect(paneHintsActive()).toBe(false);
  });

  it("reveals move destinations under the configured second layer", () => {
    preferences.modifier = "ctrl-shift";
    press("Shift", { ctrlKey: true, shiftKey: true });
    vi.advanceTimersByTime(380);
    expect(hintsActive()).toBe(true);
    press("Alt", { ctrlKey: true, shiftKey: true, altKey: true });
    expect(hintsActive()).toBe(false);
    expect(paneHintsActive()).toBe(true);
    keys.dispatchEvent(new Event("keyup"));
    expect(paneHintsActive()).toBe(false);
  });

  it("cancels pending and visible hints on blur, hiding and teardown", () => {
    press("Meta", { metaKey: true });
    keys.dispatchEvent(new Event("blur"));
    vi.advanceTimersByTime(380);
    expect(hintsActive()).toBe(false);
    press("Meta", { metaKey: true });
    vi.advanceTimersByTime(380);
    visibility.dispatchEvent(new Event("visibilitychange"));
    expect(hintsActive()).toBe(false);
    press("Meta", { metaKey: true });
    stop();
    vi.advanceTimersByTime(380);
    expect(hintsActive()).toBe(false);
  });
});
