import { describe, it, expect } from "vitest";
import {
  type Layout,
  type Tab,
  defaultLayout,
  tabKey,
  panes,
  tabCount,
  allSessionIds,
  allFilePaths,
  focusedSession,
  focusedFile,
  openSession,
  openFile,
  openGit,
  openChanges,
  splitPane,
  closePane,
  pruneSessions,
  pruneFiles,
  serializeLayout,
  deserializeLayout,
} from "./layout";

// Pure layout-tree logic — the single most refactor-fragile pure module in the
// UI, and the client has no other tests. These pin the observable behavior so a
// lib/ reorg or a layout-logic change can't silently regress it.

describe("defaultLayout", () => {
  it("is one empty focused pane, no zoom", () => {
    const l = defaultLayout();
    expect(panes(l.root)).toHaveLength(1);
    expect(tabCount(l)).toBe(0);
    expect(l.zoomedPaneId).toBeNull();
    expect(l.focusMode).toBe(false);
    expect(l.focusedPaneId).toBe(panes(l.root)[0].id);
  });
});

describe("tabKey", () => {
  it("gives each surface a distinct, stable key (the no-duplicates namespace)", () => {
    expect(tabKey({ surface: "terminal", sessionId: "s1" })).toBe("s:s1");
    expect(tabKey({ surface: "file", path: "/a.txt" })).toBe("f:/a.txt");
    expect(tabKey({ surface: "finder", id: "d1", path: "/x" } as Tab)).toBe("d:d1");
    expect(tabKey({ surface: "git" })).toBe("v:git");
    expect(tabKey({ surface: "changes", sessionId: "s1" })).toBe("changes:s1");
    expect(tabKey({ surface: "settings" })).toBe("v:settings");
    // diff uses `g:` (NOT `d:`) so it can't alias a Finder in the dedupe set.
    expect(tabKey({ surface: "diff", path: "/a", mode: "head" } as unknown as Tab)).toBe(
      "g:head:/a",
    );
  });
});

describe("opening surfaces", () => {
  it("openSession adds a terminal tab and focuses it", () => {
    const l = openSession(defaultLayout(), "s1");
    expect(allSessionIds(l)).toEqual(["s1"]);
    expect(tabCount(l)).toBe(1);
    expect(focusedSession(l)).toBe("s1");
  });

  it("dedupes: opening the same session twice keeps one tab (VS Code semantics)", () => {
    let l = openSession(defaultLayout(), "s1");
    l = openSession(l, "s1");
    expect(allSessionIds(l)).toEqual(["s1"]);
    expect(tabCount(l)).toBe(1);
  });

  it("openFile adds a file tab, focusedFile reflects the active tab", () => {
    const l = openFile(defaultLayout(), "/src/main.rs");
    expect(allFilePaths(l)).toEqual(["/src/main.rs"]);
    expect(focusedFile(l)).toBe("/src/main.rs");
    expect(focusedSession(l)).toBeNull();
  });

  it("different surfaces coexist in one pane", () => {
    let l = openSession(defaultLayout(), "s1");
    l = openFile(l, "/a.txt");
    l = openGit(l);
    expect(tabCount(l)).toBe(3);
    expect(allSessionIds(l)).toEqual(["s1"]);
    expect(allFilePaths(l)).toEqual(["/a.txt"]);
  });
});

describe("splitPane / closePane", () => {
  it("split creates two panes, preserves tab count, focuses the new empty pane", () => {
    const l0 = openSession(defaultLayout(), "s1");
    const l1 = splitPane(l0, l0.focusedPaneId, "row");
    expect(panes(l1.root)).toHaveLength(2);
    expect(tabCount(l1)).toBe(1); // the session tab is untouched
    expect(focusedSession(l1)).toBeNull(); // focus moved to the fresh empty pane
    expect(l1.zoomedPaneId).toBeNull();
  });

  it("closePane is a no-op with a single pane", () => {
    const l = openSession(defaultLayout(), "s1");
    expect(closePane(l, l.focusedPaneId)).toBe(l);
  });

  it("closing one pane collapses the split and keeps the sibling's tabs", () => {
    const l0 = openSession(defaultLayout(), "s1");
    const origPane = l0.focusedPaneId;
    const l1 = splitPane(l0, origPane, "row"); // new empty pane is focused
    const l2 = closePane(l1, l1.focusedPaneId); // close the empty one
    expect(panes(l2.root)).toHaveLength(1);
    expect(allSessionIds(l2)).toEqual(["s1"]);
  });
});

describe("pruning dead tabs", () => {
  it("pruneSessions drops terminal + changes tabs whose session is not live", () => {
    let l = openSession(defaultLayout(), "s1");
    l = openSession(l, "s2");
    l = openChanges(l, "s2");
    const pruned = pruneSessions(l, new Set(["s1"]));
    expect(allSessionIds(pruned)).toEqual(["s1"]);
    // the s2 changes tab is gone too
    expect(
      panes(pruned.root).some((p) => p.tabs.some((t) => t.surface === "changes")),
    ).toBe(false);
  });

  it("pruneFiles drops only the known-dead file paths", () => {
    let l = openFile(defaultLayout(), "/a.txt");
    l = openFile(l, "/b.txt");
    const pruned = pruneFiles(l, new Set(["/a.txt"]));
    expect(allFilePaths(pruned)).toEqual(["/b.txt"]);
  });
});

describe("serialize / deserialize round-trip", () => {
  it("preserves the observable content of a non-trivial layout", () => {
    let l: Layout = openSession(defaultLayout(), "s1");
    l = openFile(l, "/a.txt");
    l = splitPane(l, l.focusedPaneId, "col");
    l = openSession(l, "s2");
    const restored = deserializeLayout(serializeLayout(l));
    expect(restored).not.toBeNull();
    expect(new Set(allSessionIds(restored!))).toEqual(new Set(["s1", "s2"]));
    expect(allFilePaths(restored!)).toEqual(["/a.txt"]);
    expect(tabCount(restored!)).toBe(tabCount(l));
    expect(panes(restored!.root)).toHaveLength(panes(l.root).length);
  });

  it("returns null for garbage input", () => {
    expect(deserializeLayout(null)).toBeNull();
    expect(deserializeLayout(42)).toBeNull();
    expect(deserializeLayout({ nope: true })).toBeNull();
    expect(deserializeLayout("not a layout")).toBeNull();
  });
});
