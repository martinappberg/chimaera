import { describe, expect, it } from "vitest";

import { attemptLanded, attemptShown, historyAttempt } from "./agentSignIn";
import type { OperationProgress } from "./operations";

const op = (id: string, phase: OperationProgress["phase"]): OperationProgress => ({
  id,
  phase,
  workspace_id: "w",
  started_at: 0,
  message: "",
});

describe("which sign-in attempts a card reports on", () => {
  it("never the daemon's kept result of an earlier sign-in", () => {
    // A card opened by a second /login: the agent's last operation is the
    // first sign-in, succeeded long ago. It is history, so the card goes on
    // to start a login instead of announcing "signed in" and closing.
    const kept = op("first", "succeeded");
    const history = historyAttempt(kept);
    expect(history).toBe("first");
    expect(attemptShown(history, kept)).toBe(false);
    expect(attemptLanded(history, kept)).toBe(false);
  });

  it("the attempt started after it, through to its end", () => {
    const history = historyAttempt(op("first", "succeeded"));
    expect(attemptShown(history, op("mine", "running"))).toBe(true);
    expect(attemptLanded(history, op("mine", "running"))).toBe(false);
    expect(attemptLanded(history, op("mine", "succeeded"))).toBe(true);
    expect(attemptShown(history, op("mine", "failed"))).toBe(true);
    expect(attemptLanded(history, op("mine", "failed"))).toBe(false);
    expect(attemptLanded(history, op("mine", "cancelled"))).toBe(false);
  });

  it("a running attempt the card found is its to show", () => {
    // A second window, or a repeated click: the one operation per agent.
    const found = op("theirs", "running");
    const history = historyAttempt(found);
    expect(history).toBeNull();
    expect(attemptShown(history, found)).toBe(true);
    expect(attemptLanded(history, op("theirs", "succeeded"))).toBe(true);
  });

  it("a sign-in finished elsewhere since the card opened counts", () => {
    // This card's attempt failed; the user signed in from Settings and came
    // back to Check again.
    const history = historyAttempt(op("old", "cancelled"));
    expect(attemptLanded(history, op("mine", "failed"))).toBe(false);
    expect(attemptLanded(history, op("settings", "succeeded"))).toBe(true);
  });

  it("nothing to report without an operation", () => {
    expect(historyAttempt(null)).toBeNull();
    expect(historyAttempt(undefined)).toBeNull();
    expect(attemptShown(null, null)).toBe(false);
    expect(attemptLanded(null, undefined)).toBe(false);
  });
});
