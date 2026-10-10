import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { refusalFor, refusalText, refuse, setTerminalStatus, statusText, terminalStatus } from "./refusals.svelte";

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

it("every refusal reason reads differently and none is a wire code", () => {
  const texts = ["watching", "busy", "reconnecting", "elsewhere", "waking"].map((reason) =>
    refusalText(reason, null),
  );
  expect(new Set(texts).size).toBe(texts.length);
  for (const text of texts) expect(text).not.toContain("_");
  // An older daemon without `reason`: its code-like message is translated.
  expect(refusalText(null, "workspace_owned_elsewhere")).not.toContain("_");
  expect(refusalText(null, null).length).toBeGreaterThan(0);
});

it("a refusal shows inline for a few seconds, then clears itself", () => {
  refuse("s-term", "busy", null);
  expect(refusalFor("s-term")).toBe(refusalText("busy", null));
  expect(refusalFor("s-other")).toBeNull();
  vi.advanceTimersByTime(5000);
  expect(refusalFor("s-term")).toBeNull();
});

it("a waking terminal says so until it is live; a refusal shows over it meanwhile", () => {
  setTerminalStatus("s-wake", "waking");
  const waking = refusalFor("s-wake");
  expect(waking).not.toBeNull();
  refuse("s-wake", "waking", null);
  expect(refusalFor("s-wake")).toBe(refusalText("waking", null));
  vi.advanceTimersByTime(5000);
  expect(refusalFor("s-wake")).toBe(waking);
  setTerminalStatus("s-wake", null);
  expect(refusalFor("s-wake")).toBeNull();
});

it("running elsewhere names the cloud or your computer, and another device never", () => {
  expect(refusalText("elsewhere", null, "cloud")).toBe("This project is running in the cloud right now.");
  expect(refusalText("elsewhere", null, "computer")).toBe("This project is running on your computer right now.");
  expect(refusalText("elsewhere", null)).toBe("This project is running somewhere else right now.");
  refuse("s-routed", "elsewhere", null);
  // A routed pane names no machine rather than a stale one.
  expect(refusalFor("s-routed", { where: null })).toBe("This project is running somewhere else right now.");
  for (const where of ["cloud", "computer", "other", null] as const) {
    expect(refusalText("elsewhere", null, where)).not.toMatch(/device/);
  }
});

it("a terminal idle in the cloud says a keystroke continues it, until it is live again", () => {
  setTerminalStatus("s-asleep", "asleep");
  expect(terminalStatus("s-asleep")).toBe("asleep");
  expect(refusalFor("s-asleep")).toBe("Idle in your cloud. Press a key to continue.");
  expect(refusalFor("s-asleep", { watching: true })).toBe(statusText("asleep", true));
  expect(statusText("asleep", true)).toContain("Type here");
  setTerminalStatus("s-asleep", "waking");
  expect(terminalStatus("s-asleep")).toBe("waking");
  setTerminalStatus("s-asleep", null);
  expect(refusalFor("s-asleep")).toBeNull();
  expect(terminalStatus("s-asleep")).toBeNull();
});
