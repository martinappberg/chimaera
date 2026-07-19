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
  openDashboard,
  openChanges,
  splitPane,
  closePane,
  pruneSessions,
  pruneFiles,
  pruneDeletedPath,
  serializeLayout,
  deserializeLayout,
  pinTab,
  pinPaths,
  movePane,
  movePaneToRootEdge,
  moveTabDirection,
  toggleZoom,
  sessionPaneId,
  MAX_PANES,
  findPane,
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

  it("the dashboard is a singleton and round-trips serialization", () => {
    let l = openDashboard(defaultLayout());
    l = openDashboard(l);
    expect(tabCount(l)).toBe(1);
    const restored = deserializeLayout(serializeLayout(l));
    expect(restored).not.toBeNull();
    const pane = panes(restored!.root)[0];
    expect(pane.tabs[0]).toEqual({ surface: "dashboard" });
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

  it("external deletion can preserve dirty descendants while pruning clean tabs", () => {
    let l = openFile(defaultLayout(), "/gone/dirty.txt");
    l = openFile(l, "/gone/clean.txt");
    l = openFile(l, "/stay.txt");
    const pruned = pruneDeletedPath(l, "/gone", new Set(["/gone/dirty.txt"]));
    expect(allFilePaths(pruned).sort()).toEqual(["/gone/dirty.txt", "/stay.txt"]);
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

function isPreview(l: Layout, path: string): boolean {
  for (const p of panes(l.root))
    for (const t of p.tabs)
      if (t.surface === "file" && t.path === path) return t.preview === true;
  return false;
}

describe("preview (italic) file tabs", () => {
  it("a preview open replaces the pane's existing preview tab in place", () => {
    let l = openFile(defaultLayout(), "/a.txt", true);
    l = openFile(l, "/b.txt", true);
    // One preview slot: /a.txt was replaced by /b.txt, not stacked.
    expect(allFilePaths(l)).toEqual(["/b.txt"]);
    expect(isPreview(l, "/b.txt")).toBe(true);
  });

  it("dedupe wins over replace: focusing an already-open path never duplicates", () => {
    let l = openFile(defaultLayout(), "/a.txt", false); // pinned
    l = openFile(l, "/a.txt", true); // preview-open the same path
    expect(allFilePaths(l)).toEqual(["/a.txt"]);
    expect(isPreview(l, "/a.txt")).toBe(false); // stays pinned
  });

  it("a pinned open never replaces a preview tab", () => {
    let l = openFile(defaultLayout(), "/a.txt", true);
    l = openFile(l, "/b.txt", false);
    expect(new Set(allFilePaths(l))).toEqual(new Set(["/a.txt", "/b.txt"]));
  });

  it("pinTab promotes a preview tab to permanent", () => {
    let l = openFile(defaultLayout(), "/a.txt", true);
    const p = panes(l.root)[0];
    l = pinTab(l, p.id, 0);
    expect(isPreview(l, "/a.txt")).toBe(false);
  });

  it("pinPaths promotes matching preview tabs (dirty edit)", () => {
    let l = openFile(defaultLayout(), "/a.txt", true);
    l = pinPaths(l, new Set(["/a.txt"]));
    expect(isPreview(l, "/a.txt")).toBe(false);
    // Same reference when nothing matches.
    expect(pinPaths(l, new Set(["/nope"]))).toBe(l);
  });

  it("the preview flag round-trips through serialization", () => {
    let l = openFile(defaultLayout(), "/a.txt", true);
    const restored = deserializeLayout(serializeLayout(l));
    expect(restored).not.toBeNull();
    expect(isPreview(restored!, "/a.txt")).toBe(true);
  });

  it("an old blob without pv deserializes as a pinned tab", () => {
    const blob = { v: 1, focused: "p", root: { t: "p", id: "p", active: 0, tabs: [{ f: "/a.txt" }] } };
    const restored = deserializeLayout(blob);
    expect(restored).not.toBeNull();
    expect(isPreview(restored!, "/a.txt")).toBe(false);
  });
});

describe("whole-pane moves", () => {
  it("movePane center merges the source pane's tabs into the target", () => {
    let l = openSession(defaultLayout(), "s1");
    const left = l.focusedPaneId;
    l = splitPane(l, left, "row");
    const right = l.focusedPaneId;
    l = openSession(l, "s2");
    l = movePane(l, right, left, "center");
    expect(panes(l.root)).toHaveLength(1);
    expect(new Set(allSessionIds(l))).toEqual(new Set(["s1", "s2"]));
  });

  it("movePane to an edge re-parents the pane keeping its id and tabs", () => {
    let l = openSession(defaultLayout(), "s1");
    const left = l.focusedPaneId;
    l = splitPane(l, left, "row");
    const right = l.focusedPaneId;
    l = openSession(l, "s2");
    l = movePane(l, right, left, "bottom");
    expect(panes(l.root)).toHaveLength(2);
    // The moved pane kept its id (it was re-parented, not recreated).
    expect(findPane(l.root, right)).not.toBeNull();
  });

  it("self-drop and single-pane moves are no-ops", () => {
    let l = openSession(defaultLayout(), "s1");
    const only = l.focusedPaneId;
    expect(movePaneToRootEdge(l, only, "left")).toBe(l); // last pane never moves
    l = splitPane(l, only, "row");
    const before = l;
    expect(movePane(l, l.focusedPaneId, l.focusedPaneId, "center")).toBe(before); // self
  });
});

describe("moveTabDirection (shift+cmd+arrow)", () => {
  it("auto-splits a single-tab pane into a fresh empty pane on that side, focused", () => {
    let l = openSession(defaultLayout(), "s1");
    const src = l.focusedPaneId;
    l = moveTabDirection(l, "right");
    expect(panes(l.root)).toHaveLength(2);
    expect(l.focusedPaneId).not.toBe(src); // the new pane is focused
    expect(findPane(l.root, l.focusedPaneId)?.tabs).toHaveLength(0); // and empty
    expect(sessionPaneId(l, "s1")).toBe(src); // the sole tab stayed put
  });

  it("tears the ACTIVE tab of a multi-tab pane into the new pane", () => {
    let l = openSession(defaultLayout(), "t1");
    l = openSession(l, "t2"); // both in one pane; t2 is active
    const src = l.focusedPaneId;
    l = moveTabDirection(l, "right");
    expect(panes(l.root)).toHaveLength(2);
    expect(sessionPaneId(l, "t2")).toBe(l.focusedPaneId); // t2 moved into the new pane
    expect(sessionPaneId(l, "t1")).toBe(src); // t1 stayed behind
  });

  it("moves into an existing neighbor instead of splitting", () => {
    let l = openSession(defaultLayout(), "a");
    const paneA = l.focusedPaneId;
    l = splitPane(l, paneA, "row"); // A | B (empty, focused)
    l = openSession(l, "b1");
    l = openSession(l, "b2"); // B = [b1, b2], b2 active
    const count = panes(l.root).length; // 2
    l = moveTabDirection(l, "left"); // neighbor A exists → move b2 into A (B keeps b1)
    expect(panes(l.root)).toHaveLength(count); // no new pane
    expect(sessionPaneId(l, "b2")).toBe(paneA); // moved into the neighbor
  });

  it("never auto-splits past MAX_PANES", () => {
    let l = openSession(defaultLayout(), "c0");
    for (let i = 0; i < MAX_PANES + 3; i++) l = moveTabDirection(l, "right");
    expect(panes(l.root).length).toBeLessThanOrEqual(MAX_PANES);
  });

  it("is a no-op while zoomed", () => {
    let l = openSession(defaultLayout(), "z1");
    l = toggleZoom(l);
    expect(moveTabDirection(l, "right")).toBe(l);
  });

  it("is a no-op when the focused pane is empty", () => {
    const l = defaultLayout(); // one empty pane
    expect(moveTabDirection(l, "right")).toBe(l);
  });
});
