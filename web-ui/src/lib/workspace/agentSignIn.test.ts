import { describe, expect, it } from "vitest";

import { attemptLanded, shownAttempt } from "./agentSignIn";
import type { OperationProgress } from "./operations";

const op = (id: string, phase: OperationProgress["phase"]): OperationProgress => ({
  id,
  phase,
  workspace_id: "w",
  started_at: 0,
  message: "",
});

describe("which sign-in attempt a card reports on", () => {
  it("is never the daemon's kept result of an earlier sign-in", () => {
    // A card opened by a second /login: the agent's last operation is the
    // first sign-in, succeeded long ago. It is not this card's, so the card
    // goes on to start a login instead of announcing "signed in" and closing.
    const kept = op("first", "succeeded");
    const own = shownAttempt(null, kept);
    expect(own).toBeNull();
    expect(attemptLanded(own, kept)).toBe(false);
  });

  it("is the attempt the card started, through to its end", () => {
    expect(attemptLanded("mine", op("mine", "running"))).toBe(false);
    expect(shownAttempt("mine", op("mine", "succeeded"))).toBe("mine");
    expect(attemptLanded("mine", op("mine", "succeeded"))).toBe(true);
    expect(attemptLanded("mine", op("mine", "failed"))).toBe(false);
    expect(attemptLanded("mine", op("mine", "cancelled"))).toBe(false);
  });

  it("adopts a running attempt the card found and shows", () => {
    // A second window, or a repeated click: the one operation per agent.
    const own = shownAttempt(null, op("theirs", "running"));
    expect(own).toBe("theirs");
    expect(shownAttempt(own, op("theirs", "cancelling"))).toBe("theirs");
    expect(attemptLanded(own, op("theirs", "succeeded"))).toBe(true);
  });

  it("moves to a newer running attempt and forgets nothing without one", () => {
    expect(shownAttempt("mine", op("newer", "running"))).toBe("newer");
    expect(shownAttempt("mine", null)).toBe("mine");
    expect(shownAttempt(null, undefined)).toBeNull();
    expect(attemptLanded(null, null)).toBe(false);
  });
});
