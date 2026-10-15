import { describe, expect, it } from "vitest";
import { parseRecents } from "./launcher";
import { wantsWorktree, worktreeInUse, type Session } from "./sessions";

const on = { new_in_worktree: true };

describe("wantsWorktree", () => {
  it("a new agent in a git workspace with the choice on", () => {
    expect(wantsWorktree("agent", { agent: "codex" }, on, true)).toBe(true);
    expect(wantsWorktree("agent", {}, on, true)).toBe(true);
  });
  it("never for shells, non-repos, or when off/absent", () => {
    expect(wantsWorktree("shell", {}, on, true)).toBe(false);
    expect(wantsWorktree("agent", {}, on, false)).toBe(false);
    expect(wantsWorktree("agent", {}, { new_in_worktree: false }, true)).toBe(false);
    expect(wantsWorktree("agent", {}, {}, true)).toBe(false);
    expect(wantsWorktree("agent", {}, null, true)).toBe(false);
  });
  it("never for a resume or an explicit folder", () => {
    expect(wantsWorktree("agent", { resume: "abc" }, on, true)).toBe(false);
    expect(wantsWorktree("agent", { cwd: "/r/.wt/feat" }, on, true)).toBe(false);
    expect(wantsWorktree("agent", { cwd: "" }, on, true)).toBe(true);
  });
});

function sess(id: string, extra: Partial<Session> = {}): Session {
  return {
    id,
    name: id,
    cwd: "/repo",
    cols: 80,
    rows: 24,
    created_at: 0,
    alive: true,
    exit_status: null,
    title: null,
    workspace_id: "w",
    kind: "agent",
    agent_state: null,
    agent_title: null,
    ...extra,
  };
}

describe("worktreeInUse", () => {
  const wt = "/home/u/.chimaera/worktrees/repo-1a2b/chimaera/claude-abc123";
  it("ignores the ending session itself", () => {
    const list = [sess("a", { managed_worktree: wt, cwd: wt })];
    expect(worktreeInUse(list, wt, "a")).toBe(false);
  });
  it("sees another live session by spawn folder, cwd or checkout", () => {
    expect(worktreeInUse([sess("b", { managed_worktree: wt })], wt, "a")).toBe(true);
    expect(worktreeInUse([sess("b", { cwd: `${wt}/src` })], wt, "a")).toBe(true);
    expect(worktreeInUse([sess("b", { cwd_current: wt })], wt, "a")).toBe(true);
    expect(
      worktreeInUse(
        [sess("b", { git: { repo: "/repo", worktree: wt, branch: "x", detached: false, head: null } })],
        wt,
        "a",
      ),
    ).toBe(true);
  });
  it("a sibling prefix or an exited session does not count", () => {
    expect(worktreeInUse([sess("b", { cwd: `${wt}-2` })], wt, "a")).toBe(false);
    expect(worktreeInUse([sess("b", { managed_worktree: wt, alive: false })], wt, "a")).toBe(false);
  });
});

describe("parseRecents worktree", () => {
  it("parses the managed worktree, tolerating old daemons", () => {
    const rows = parseRecents([
      { kind: "claude", title: "t", resume: "r1", worktree: "/wt/a" },
      { kind: "codex", title: "u", resume: "r2" },
      { kind: "codex", title: "v", resume: "r3", worktree: null },
      { kind: "codex", title: "w", resume: "r4", worktree: 7 },
    ]);
    expect(rows.map((r) => r.worktree)).toEqual(["/wt/a", null, null, null]);
  });
});
