import { describe, expect, it } from "vitest";

import type { AgentInfo } from "../workspace/launcher";
import { canSwitchAccount, limitNotice } from "./limitCard";

const NOW = Date.UTC(2026, 10, 26, 12, 0, 0);
const inSeconds = (seconds: number) => String(Math.floor(NOW / 1000) + seconds);

describe("limitNotice", () => {
  it("says nothing while no limit is reached", () => {
    expect(limitNotice(null, NOW)).toBeNull();
    expect(limitNotice({ label: "weekly limit", resetsAt: inSeconds(3600), limitReached: false }, NOW)).toBeNull();
  });

  it("names the window in the agent's words and when it resets", () => {
    expect(limitNotice({ label: "session limit", resetsAt: inSeconds(2 * 3600 + 14 * 60), limitReached: true }, NOW)).toEqual({
      what: "Session limit reached",
      resets: "resets in 2h 14m",
    });
    expect(limitNotice({ label: "weekly Opus limit", resetsAt: inSeconds(40 * 60), limitReached: true }, NOW)).toEqual({
      what: "Weekly Opus limit reached",
      resets: "resets in 40m",
    });
  });

  it("reads an ISO reset time like an epoch one", () => {
    const iso = new Date(NOW + 3 * 3600_000).toISOString();
    expect(limitNotice({ label: "session limit", resetsAt: iso, limitReached: true }, NOW)?.resets).toBe("resets in 3h");
  });

  it("uses the agents' generic words when the window has no name", () => {
    expect(limitNotice({ label: null, resetsAt: null, limitReached: true }, NOW)).toEqual({
      what: "Usage limit reached",
      resets: null,
    });
  });

  it("drops a reset time that has passed, and keeps the line", () => {
    // Only the agent says a limit is over; the clock never does.
    expect(limitNotice({ label: "session limit", resetsAt: inSeconds(-60), limitReached: true }, NOW)).toEqual({
      what: "Session limit reached",
      resets: null,
    });
    expect(limitNotice({ label: "session limit", resetsAt: "not a time", limitReached: true }, NOW)?.resets).toBeNull();
  });
});

describe("canSwitchAccount", () => {
  const agent = (over: Partial<AgentInfo>): AgentInfo =>
    ({ id: "x", name: "X", installed: true, signedIn: true, signIn: { how: "browser", command: "x login", acceptsCode: false, relogin: true }, ...over }) as AgentInfo;

  it("follows the daemon's sign-in descriptor, never the agent's name", () => {
    expect(canSwitchAccount(agent({}))).toBe(true);
    // No sign-in from the app.
    expect(canSwitchAccount(agent({ signIn: undefined }))).toBe(false);
    // A login that only finds the existing account.
    expect(canSwitchAccount(agent({ signIn: { how: "browser", command: "x", acceptsCode: false, relogin: false } }))).toBe(false);
    expect(canSwitchAccount(agent({ installed: false }))).toBe(false);
    expect(canSwitchAccount(null)).toBe(false);
  });
});
