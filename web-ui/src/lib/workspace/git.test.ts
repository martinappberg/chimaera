import { describe, expect, it } from "vitest";
import { buildIndex, repoForPath, type GitEntry, type GitRepo, type GitStatus } from "./git";

function entry(path: string): GitEntry {
  return {
    path,
    rel: path,
    orig: null,
    orig_rel: null,
    x: " ",
    y: "M",
    staged: false,
    unstaged: true,
    untracked: false,
    conflicted: false,
  } as GitEntry;
}

function status(toplevel: string, paths: string[]): GitStatus {
  return { repo: true, toplevel, entries: paths.map(entry) } as unknown as GitStatus;
}

function repo(path: string, kind: GitRepo["kind"] = "nested"): GitRepo {
  return { path, rel: null, kind, submodule: false, parent: null, branch: null } as GitRepo;
}

describe("git folder rollup", () => {
  it("marks every ancestor up to the repository, never the filesystem root (Unix)", () => {
    const idx = buildIndex(status("/w", ["/w/a/b/c.txt"]), new Map(), []);
    expect([...idx.dirs.keys()].sort()).toEqual(["/w", "/w/a", "/w/a/b"]);
  });

  it("does the same on a Windows daemon's drive paths, never naming the drive", () => {
    const idx = buildIndex(status("C:/w", ["C:/w/a/b/c.txt"]), new Map(), []);
    expect([...idx.dirs.keys()].sort()).toEqual(["C:/w", "C:/w/a", "C:/w/a/b"]);
    const top = buildIndex(status("C:/", ["C:/a/b.txt"]), new Map(), []);
    expect([...top.dirs.keys()]).toEqual(["C:/a"]);
  });
});

describe("repoForPath", () => {
  const repos = [repo("/w", "root"), repo("/w/sub"), repo("C:/w", "root"), repo("C:/w/sub")];

  it("picks the innermost repository holding the path", () => {
    expect(repoForPath(repos, "/w/sub/x.rs")?.path).toBe("/w/sub");
    expect(repoForPath(repos, "/w/subway/x.rs")?.path).toBe("/w");
    expect(repoForPath(repos, "C:/w/sub/x.rs")?.path).toBe("C:/w/sub");
    expect(repoForPath(repos, "C:/w/subway/x.rs")?.path).toBe("C:/w");
    expect(repoForPath(repos, "D:/w/x.rs")).toBeNull();
    expect(repoForPath(repos, "C:/w")?.path).toBe("C:/w");
  });
});
