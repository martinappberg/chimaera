import { describe, expect, it } from "vitest";
import { sessionSurface, dotTitle, type Session } from "./sessions";

describe("conversation surface survives updates", () => {
  for (const agent of ["claude", "codex", "agy", "grok", "plugin:pi"]) {
    it(`${agent}: reopening ignores new-chat defaults and readiness changes`, () => {
      for (const defaultChat of [true, false]) {
        for (const chatCapable of [true, false, undefined]) {
          expect(sessionSurface({agent, resume: "saved", ui: "term"}, defaultChat, chatCapable)).toBe("term");
          expect(sessionSurface({agent, resume: "saved", ui: "chat"}, defaultChat, chatCapable)).toBe("chat");
          expect(sessionSurface({agent, resume: "legacy"}, defaultChat, chatCapable)).toBe("term");
        }
      }
    });
  }
  it("only fresh implicit launches inherit the default", () => {
    expect(sessionSurface({}, true, true)).toBe("chat");
    expect(sessionSurface({}, false, true)).toBe("term");
    expect(sessionSurface({}, true, false)).toBe("term");
    expect(sessionSurface({ui: "chat"}, false, false)).toBe("chat");
  });
});


it("an unreported terminal state does not claim perpetual startup", () => {
  for (const agent_kind of ["claude", "codex", "agy", "grok", "plugin:pi"]) {
    const session = { kind: "agent", alive: true, ui: "term", agent_kind, agent_state: "unknown" } as Session;
    expect(dotTitle(session)).toBe("terminal open");
  }
});
