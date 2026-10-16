import { describe, expect, it } from "vitest";
import type { Session } from "./sessions";
import {
  agentSetChange,
  dropTombstoned,
  needsAppend,
  SessionRoster,
  sortByFirstCreated,
} from "./sessionRoster";

function s(id: string, created_at: number, kind: "agent" | "shell" = "shell"): Session {
  return { id, created_at, kind } as Session;
}
const ids = (list: readonly Session[]) => list.map((x) => x.id);

describe("dropTombstoned", () => {
  it("keeps a killed session hidden while the daemon still reports it", () => {
    const killing = new Set(["b"]);
    const out = dropTombstoned([s("a", 1), s("b", 2)], killing, true);
    expect(ids(out)).toEqual(["a"]);
    expect([...killing]).toEqual(["b"]);
  });

  it("lifts the tombstone once a daemon snapshot omits the id", () => {
    const killing = new Set(["b"]);
    const out = dropTombstoned([s("a", 1)], killing, true);
    expect(ids(out)).toEqual(["a"]);
    expect(killing.size).toBe(0);
    // A later resume under the same id is not filtered away.
    expect(ids(dropTombstoned([s("a", 1), s("b", 3)], killing, true))).toEqual(["a", "b"]);
  });

  it("a local (non-snapshot) list never lifts its own tombstone", () => {
    const killing = new Set(["b"]);
    expect(ids(dropTombstoned([s("a", 1)], killing, false))).toEqual(["a"]);
    expect([...killing]).toEqual(["b"]);
  });

  it("returns the same list when nothing is tombstoned", () => {
    const list = [s("a", 1)];
    expect(dropTombstoned(list, new Set(), true)).toBe(list);
  });
});

describe("sortByFirstCreated", () => {
  it("orders by created_at, ties by id", () => {
    const out = sortByFirstCreated([s("c", 5), s("b", 5), s("a", 9)], new Map());
    expect(ids(out)).toEqual(["b", "c", "a"]);
  });

  it("keeps a switching session's place through a created_at:0 placeholder and respawn", () => {
    const seen = new Map<string, number>();
    sortByFirstCreated([s("a", 10), s("b", 20)], seen);
    expect(ids(sortByFirstCreated([s("a", 10), s("b", 0)], seen))).toEqual(["a", "b"]);
    expect(ids(sortByFirstCreated([s("b", 30), s("a", 10)], seen))).toEqual(["a", "b"]);
    expect(seen.get("b")).toBe(20);
  });

  it("sorts a never-seen placeholder by its sentinel 0 (top)", () => {
    expect(ids(sortByFirstCreated([s("a", 10), s("z", 0)], new Map()))).toEqual(["z", "a"]);
  });

  it("forgets ids no longer listed", () => {
    const seen = new Map<string, number>();
    sortByFirstCreated([s("a", 10), s("b", 20)], seen);
    sortByFirstCreated([s("a", 10)], seen);
    expect([...seen.keys()]).toEqual(["a"]);
    // b comes back as a new session with its new created_at.
    expect(ids(sortByFirstCreated([s("b", 5), s("a", 10)], seen))).toEqual(["b", "a"]);
  });

  it("keeps duplicate ids (both rows) and records the first non-zero created_at", () => {
    const seen = new Map<string, number>();
    const out = sortByFirstCreated([s("d", 7), s("a", 3), s("d", 1)], seen);
    expect(ids(out)).toEqual(["a", "d", "d"]);
    expect(seen.get("d")).toBe(7);
  });
});

describe("agentSetChange", () => {
  it("is a set comparison over agent rows only", () => {
    const prev = new Set(["x"]);
    expect(agentSetChange(prev, [s("x", 1, "agent"), s("sh", 2)]).changed).toBe(false);
    expect(agentSetChange(prev, [s("y", 1, "agent")]).changed).toBe(true);
    expect(agentSetChange(prev, []).changed).toBe(true);
    expect([...agentSetChange(new Set(), [s("y", 1, "agent"), s("sh", 2)]).ids]).toEqual(["y"]);
  });
});

describe("needsAppend", () => {
  it("does not duplicate a session the daemon reported before its create ack", () => {
    const list = [s("a", 1)];
    expect(needsAppend(list, s("a", 1))).toBe(false);
    expect(needsAppend(list, s("b", 2))).toBe(true);
  });
});

describe("SessionRoster", () => {
  it("reconciles: tombstones, order, and the agent-set change", () => {
    const r = new SessionRoster();
    const first = r.reconcile([s("b", 2, "agent"), s("a", 1)], true);
    expect(ids(first.list)).toEqual(["a", "b"]);
    expect(first.agentsChanged).toBe(true);
    expect(r.reconcile([s("a", 1), s("b", 2, "agent")], true).agentsChanged).toBe(false);
    r.bury(["b"]);
    const killed = r.reconcile([s("a", 1), s("b", 2, "agent")], true);
    expect(ids(killed.list)).toEqual(["a"]);
    expect(killed.agentsChanged).toBe(true);
  });

  it("expire reports a tombstone no snapshot confirmed", () => {
    const r = new SessionRoster();
    r.bury(["a", "b"]);
    r.reconcile([s("a", 1)], true); // b confirmed gone
    expect(r.expire(["a", "b"])).toBe(true);
    expect(r.expire(["a", "b"])).toBe(false);
  });
});
