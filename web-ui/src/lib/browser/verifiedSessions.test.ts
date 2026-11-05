import { describe, expect, it } from "vitest";
import { VerifiedSessions } from "./verifiedSessions";

describe("VerifiedSessions (a remounted pane shows its page at once)", () => {
  it("answers a fresh session for its own target only", () => {
    let now = 0;
    const v = new VerifiedSessions(1000, () => now);
    v.remember("LocalHost", 8888, { id: "p1", base: "/proxy/p1" });
    expect(v.recent("localhost", 8888)).toEqual({ id: "p1", base: "/proxy/p1" });
    expect(v.recent("localhost", 8889)).toBeNull();
    now = 1000;
    expect(v.recent("localhost", 8888)?.id).toBe("p1");
  });

  it("drops a stale or forgotten session", () => {
    let now = 0;
    const v = new VerifiedSessions(1000, () => now);
    v.remember("localhost", 8888, { id: "p1", base: "/proxy/p1" });
    now = 1001;
    expect(v.recent("localhost", 8888)).toBeNull();
    v.remember("localhost", 8888, { id: "p2", base: "/proxy/p2" });
    v.forget("localhost", 8888);
    expect(v.recent("localhost", 8888)).toBeNull();
  });
});
