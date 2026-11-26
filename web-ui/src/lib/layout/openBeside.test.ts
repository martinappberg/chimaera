import { describe, expect, it } from "vitest";
import {
  MAX_PANES,
  defaultLayout,
  focusBeside,
  focusPane,
  openFile,
  openSession,
  opensBesideSession,
  panes,
  splitPane,
  toggleZoom,
  type Layout,
} from "./layout";

/** A chat on the left and a file on the right. */
function chatAndFile(): { l: Layout; chat: string; right: string } {
  let l = openSession(defaultLayout(), "agent");
  const chat = l.focusedPaneId;
  l = splitPane(l, chat, "row");
  const right = l.focusedPaneId;
  l = openFile(l, "/notes.md", false);
  return { l: focusPane(l, chat), chat, right };
}

describe("focusBeside (where an open-beside gesture lands)", () => {
  it("splits to the right when the pane is alone", () => {
    const l = openSession(defaultLayout(), "agent");
    const from = l.focusedPaneId;
    const next = focusBeside(l, from);
    expect(panes(next.root)).toHaveLength(2);
    expect(next.root).toMatchObject({ type: "split", dir: "row", a: { id: from } });
    expect(next.focusedPaneId).not.toBe(from);
  });

  it("reuses the neighbor instead of splitting again", () => {
    const { l, chat, right } = chatAndFile();
    const once = focusBeside(l, chat);
    expect(once.focusedPaneId).toBe(right);
    expect(panes(once.root)).toHaveLength(2);
    const twice = focusBeside(focusPane(once, chat), chat);
    expect(panes(twice.root)).toHaveLength(2);
  });

  it("never goes past the pane cap", () => {
    let l = openSession(defaultLayout(), "agent");
    const first = l.focusedPaneId;
    while (panes(l.root).length < MAX_PANES) l = splitPane(l, l.focusedPaneId, "row");
    const next = focusBeside(focusPane(l, first), first);
    expect(panes(next.root)).toHaveLength(MAX_PANES);
    expect(next.focusedPaneId).not.toBe(first);
  });

  it("clears a zoom so both panes show", () => {
    const { l, chat, right } = chatAndFile();
    const next = focusBeside(toggleZoom(l), chat);
    expect(next.zoomedPaneId).toBeNull();
    expect(next.focusedPaneId).toBe(right);
  });
});

describe("opensBesideSession (a plain click from a chat keeps it in view)", () => {
  it("is true from a session pane with a neighbor", () => {
    const { l, chat } = chatAndFile();
    expect(opensBesideSession(l, chat)).toBe(true);
  });

  it("is false from a session pane alone, so the open lands in it", () => {
    const l = openSession(defaultLayout(), "agent");
    expect(opensBesideSession(l, l.focusedPaneId)).toBe(false);
  });

  it("is false from a pane showing a file", () => {
    const { l, right } = chatAndFile();
    expect(opensBesideSession(l, right)).toBe(false);
  });

  it("is false while the session pane is zoomed (its neighbor is hidden)", () => {
    const { l, chat } = chatAndFile();
    expect(opensBesideSession(toggleZoom(l), chat)).toBe(false);
  });
});
