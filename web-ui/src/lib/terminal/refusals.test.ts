import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { refusalFor, refusalText, refuse, setTerminalStatus } from "./refusals.svelte";

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
