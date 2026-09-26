import { describe, expect, it } from "vitest";
import {
  defaultLayout,
  openDashboard,
  openFile,
  openKnowledge,
  openSession,
  openTab,
  splitPane,
} from "./layout";
import { SURFACES_CAP, surfacesOf } from "./surfaces";

describe("surfacesOf (the view-state `surfaces` manifest)", () => {
  it("lists every open surface in tree order with workspace-relative paths and one focused flag", () => {
    let l = openDashboard(defaultLayout());
    l = openSession(l, "s1");
    l = openFile(l, "/ws/qc/qc.R");
    l = openFile(l, "/elsewhere/notes.md");
    l = openKnowledge(l);
    const refs = surfacesOf(l, "/ws");
    expect(refs).toEqual([
      { surface: "dashboard" },
      { surface: "session", sid: "s1" },
      { surface: "file", path: "qc/qc.R" },
      { surface: "file", path: "/elsewhere/notes.md" },
      { surface: "knowledge", focused: true },
    ]);
    expect(refs.filter((r) => r.focused).length).toBe(1);
  });

  it("flags the active tab of the FOCUSED pane only", () => {
    let l = openSession(defaultLayout(), "s1");
    l = splitPane(l, l.focusedPaneId, "row");
    l = openFile(l, "/ws/a.txt");
    const refs = surfacesOf(l, "/ws");
    expect(refs).toEqual([
      { surface: "session", sid: "s1" },
      { surface: "file", path: "a.txt", focused: true },
    ]);
  });

  it("maps changes reviews to their session and never carries contents", () => {
    const l = openTab(defaultLayout(), { surface: "changes", sessionId: "s9" });
    expect(surfacesOf(l, null)).toEqual([{ surface: "changes", sid: "s9", focused: true }]);
    expect(JSON.stringify(surfacesOf(l, null))).not.toContain("content");
  });

  it("caps the list and keeps the focused entry inside the cap", () => {
    let l = defaultLayout();
    for (let i = 0; i < SURFACES_CAP + 5; i++) l = openFile(l, `/ws/f${i}.txt`);
    const refs = surfacesOf(l, "/ws");
    expect(refs.length).toBe(SURFACES_CAP);
    const focused = refs.filter((r) => r.focused);
    expect(focused).toEqual([{ surface: "file", path: `f${SURFACES_CAP + 4}.txt`, focused: true }]);
  });
});
