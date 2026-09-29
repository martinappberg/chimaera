import { describe, expect, it } from "vitest";
import {
  aggCost,
  commitCount,
  formatCost,
  formatDuration,
  formatTokens,
  isRecord,
  recordTitle,
  sameFileOverlaps,
  startedByLabel,
} from "./history";
import type { Session } from "./sessions";

function agent(id: string, ws: string, files: string[], alive = true): Session {
  return {
    id,
    name: id,
    cwd: "/w",
    cols: 80,
    rows: 24,
    created_at: 0,
    alive,
    exit_status: null,
    title: null,
    workspace_id: ws,
    kind: "agent",
    agent_state: "running",
    agent_title: null,
    files_touched: files,
  } as Session;
}

describe("honest numbers", () => {
  it("never shows an unknown cost as zero", () => {
    expect(formatCost(null)).toBe("—");
    expect(formatCost(undefined)).toBe("—");
    expect(formatCost(Number.NaN)).toBe("—");
    expect(formatCost(0)).toBe("$0.00");
    expect(formatCost(0.004)).toBe("<$0.01");
    expect(formatCost(1.234)).toBe("$1.23");
    // An aggregate with no costed session is unknown too.
    expect(aggCost({ cost_usd: 0, cost_sessions: 0 })).toBe("—");
    expect(aggCost({ cost_usd: 2.5, cost_sessions: 3 })).toBe("$2.50");
  });

  it("formats tokens and durations compactly", () => {
    expect(formatTokens(null)).toBe("—");
    expect(formatTokens(950)).toBe("950");
    expect(formatTokens(1234)).toBe("1.2k");
    expect(formatTokens(45_000)).toBe("45k");
    expect(formatTokens(1_500_000)).toBe("1.5M");
    expect(formatDuration(20_000)).toBe("<1 min");
    expect(formatDuration(4 * 60_000)).toBe("4 min");
    expect(formatDuration(80 * 60_000)).toBe("1h 20m");
    expect(formatDuration(120 * 60_000)).toBe("2h");
    expect(formatDuration(3 * 24 * 3_600_000)).toBe("3 d");
  });

  it("names who started a session", () => {
    const names = (id: string) =>
      id === "s-1" ? "fix normalization" : undefined;
    expect(startedByLabel("you", names)).toBe("you");
    expect(startedByLabel("mastermind", names)).toBe("the Mastermind");
    expect(startedByLabel("restart", names)).toBe("a restart");
    expect(startedByLabel("s-1", names)).toBe("fork of fix normalization");
    expect(startedByLabel("s-gone", names)).toBe("a fork");
  });

  it("shows commits only where git anchors exist", () => {
    expect(commitCount({ git: null })).toBeNull();
    expect(commitCount({ git: { commits: ["a", "b"] } })).toBe(2);
  });

  it("titles a record by what is known", () => {
    expect(
      recordTitle({ title: "QC", first_prompt: "fix it", agent: "claude" }),
    ).toBe("QC");
    expect(recordTitle({ first_prompt: "fix it", agent: "claude" })).toBe(
      "fix it",
    );
    expect(recordTitle({ agent: "codex" })).toBe("codex");
    expect(recordTitle({ agent: "claude", mastermind: true })).toBe("Mastermind");
  });

  it("drops malformed rows instead of the list", () => {
    expect(
      isRecord({
        rid: "a",
        id: "s",
        agent: "claude",
        started: 1,
        usage: {},
        files: { n: 0 },
      }),
    ).toBe(true);
    expect(isRecord({ rid: "a", id: "s" })).toBe(false);
    expect(isRecord(null)).toBe(false);
  });
});

describe("sameFileOverlaps", () => {
  it("pairs live sessions of one workspace that wrote the same file", () => {
    const out = sameFileOverlaps([
      agent("a", "w1", ["/w/qc.py", "/w/x.py"]),
      agent("b", "w1", ["/w/y.py", "/w/qc.py"]),
      agent("c", "w1", ["/w/z.py"]),
    ]);
    expect(out.get("a")).toEqual([{ other: "b", path: "/w/qc.py" }]);
    expect(out.get("b")).toEqual([{ other: "a", path: "/w/qc.py" }]);
    expect(out.has("c")).toBe(false);
  });

  it("ignores other workspaces, dead sessions and shells", () => {
    const shell = {
      ...agent("sh", "w1", ["/w/qc.py"]),
      kind: "shell",
    } as Session;
    const out = sameFileOverlaps([
      agent("a", "w1", ["/w/qc.py"]),
      agent("b", "w2", ["/w/qc.py"]),
      agent("d", "w1", ["/w/qc.py"], false),
      shell,
    ]);
    expect(out.size).toBe(0);
  });

  it("names the newest shared file and caps the list", () => {
    const out = sameFileOverlaps(
      [
        agent("a", "w", ["/w/1", "/w/2"]),
        agent("b", "w", ["/w/1", "/w/2"]),
        agent("c", "w", ["/w/1"]),
      ],
      1,
    );
    expect(out.get("a")).toEqual([{ other: "b", path: "/w/2" }]);
    expect(out.get("c")).toEqual([{ other: "a", path: "/w/1" }]);
  });
});
