import { describe, expect, it } from "vitest";
import {
  EMPTY_CURSOR,
  advanceCursor,
  parseSubagentChatId,
  subagentChatId,
  subagentModel,
  subagentRunning,
  type SubagentRead,
  type SubagentSource,
} from "./subagentView";

const read = (over: Partial<SubagentRead>): SubagentRead => ({
  agent: "claude",
  epoch: "0",
  from: 0,
  events: [],
  ...over,
});
const ev = (n: number) => Array.from({ length: n }, () => ({ type: "message_chunk" }));

describe("subagent view ids", () => {
  it("round-trips and never reads a session id as a subagent", () => {
    const ref = { parentId: "s-21594370", agentId: "a0dd2a5017a275850" };
    expect(parseSubagentChatId(subagentChatId(ref))).toEqual(ref);
    const codex = { parentId: "s-1", agentId: "01a10e1a-9700-7682-87cf-eb8448a31ad6" };
    expect(parseSubagentChatId(subagentChatId(codex))).toEqual(codex);
    for (const id of ["s-21594370", "sub:", "sub:s-1", "sub:s-1:", "sub::a"]) {
      expect(parseSubagentChatId(id)).toBeNull();
    }
  });
});

describe("subagent cursor", () => {
  it("starts over on the first answer and appends within a window", () => {
    const first = advanceCursor(EMPTY_CURSOR, read({ events: ev(3), stamp: "10" }));
    expect(first).toMatchObject({ restart: true, cursor: { epoch: "0", held: 3, stamp: "10" } });
    const more = advanceCursor(first!.cursor, read({ from: 3, events: ev(2), stamp: "20" }));
    expect(more).toMatchObject({ restart: false, cursor: { held: 5, stamp: "20" } });
    // Nothing new: the cursor stands.
    const same = advanceCursor(more!.cursor, read({ from: 5, stamp: "20" }));
    expect(same).toMatchObject({ restart: false, cursor: { held: 5 } });
  });

  it("starts over when the window moves", () => {
    const held = { epoch: "0", held: 40, stamp: "9" };
    const moved = advanceCursor(held, read({ epoch: "1048576", events: ev(7) }));
    expect(moved).toMatchObject({ restart: true, cursor: { epoch: "1048576", held: 7 } });
  });

  it("ignores an answer made for another cursor", () => {
    const held = { epoch: "0", held: 5, stamp: null };
    // A slow response that started from an older position.
    expect(advanceCursor(held, read({ from: 3, events: ev(2) }))).toBeNull();
    // A partial answer for a window the reader has nothing of.
    expect(advanceCursor(EMPTY_CURSOR, read({ from: 4, events: ev(1) }))).toBeNull();
  });
});

describe("subagent state from the parent chat", () => {
  const parent = (over: Partial<SubagentSource>): SubagentSource => ({
    backgroundTasks: [],
    activeAgents: [],
    subagents: new Map(),
    ...over,
  });
  const info = (agentId: string, model: string | null) => ({ agentId, model, agentType: null });

  it("is running while a lane or an in-flight row names it", () => {
    const lane = parent({ backgroundTasks: [{ id: "a1", model: "m-lane" }] as never });
    expect(subagentRunning(lane, "a1")).toBe(true);
    expect(subagentRunning(lane, "a2")).toBe(false);
    const row = parent({
      activeAgents: [{ id: "tu-1" }] as never,
      subagents: new Map([["tu-1", info("a2", "m-row")]]),
    });
    expect(subagentRunning(row, "a2")).toBe(true);
    // A finished row keeps its facts but is no longer running.
    const done = parent({ subagents: new Map([["tu-1", info("a2", "m-row")]]) });
    expect(subagentRunning(done, "a2")).toBe(false);
    expect(subagentModel(done, "a2")).toBe("m-row");
    expect(subagentModel(lane, "a1")).toBe("m-lane");
    expect(subagentModel(lane, "nobody")).toBeNull();
  });
});
