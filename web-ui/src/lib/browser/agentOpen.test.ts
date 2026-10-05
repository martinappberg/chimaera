import { describe, expect, it } from "vitest";
import {
  type Layout,
  MAX_PANES,
  activateTab,
  defaultLayout,
  findPane,
  freshBrowserTab,
  openSession,
  openTab,
  panes,
  sessionPaneId,
  splitPane,
} from "../layout/layout";
import {
  type AgentBrowserOpen,
  parseAgentBrowserOpen,
  placeAgentBrowser,
  shouldActOnAgentBrowserOpen,
} from "./agentOpen";

function frame(over: Partial<AgentBrowserOpen> = {}): AgentBrowserOpen {
  return {
    sessionId: "s-agent",
    workspaceId: "w-1",
    host: "localhost",
    port: 5173,
    path: "/",
    ...over,
  };
}

/** A window with the agent's chat in one pane (focused). */
function withAgent(): Layout {
  return openSession(defaultLayout(), "s-agent");
}

function browserTabs(l: Layout) {
  return panes(l.root).flatMap((p) =>
    p.tabs.filter((t) => t.surface === "browser").map((t) => ({ pane: p.id, tab: t })),
  );
}

function activeTab(l: Layout, paneId: string) {
  const p = findPane(l.root, paneId);
  return p?.tabs[p.active];
}

describe("parseAgentBrowserOpen", () => {
  it("accepts the daemon's frame shape", () => {
    expect(
      parseAgentBrowserOpen({
        type: "browser_open",
        session_id: "s-1",
        workspace_id: "w-1",
        host: "localhost",
        port: 5173,
        path: "/app?x=1#top",
      }),
    ).toEqual({
      sessionId: "s-1",
      workspaceId: "w-1",
      host: "localhost",
      port: 5173,
      path: "/app?x=1#top",
    });
  });

  it("drops malformed frames instead of throwing", () => {
    const ok = { session_id: "s-1", workspace_id: null, host: "h", port: 1, path: "/" };
    expect(parseAgentBrowserOpen(ok)?.workspaceId).toBeNull();
    for (const bad of [
      null,
      "x",
      { ...ok, session_id: "" },
      { ...ok, host: 3 },
      { ...ok, port: 0 },
      { ...ok, port: 70000 },
      { ...ok, port: 1.5 },
      { ...ok, path: "relative" },
    ]) {
      expect(parseAgentBrowserOpen(bad)).toBeNull();
    }
  });

  it("normalizes the path like a clicked URL, never escaping the proxy prefix", () => {
    const parse = (path: string) =>
      parseAgentBrowserOpen({ session_id: "s", host: "h", port: 1, path })?.path;
    expect(parse("/a/../../api/v1/x")).toBe("/api/v1/x");
    expect(parse("/a/%2e%2e/b")).toBe("/b");
    expect(parse("//evil.example/x")).toBe("//evil.example/x");
    expect(parse("/a b")).toBe("/a%20b");
  });
});

describe("shouldActOnAgentBrowserOpen", () => {
  it("the window holding the session acts, visible or not, whatever it shows", () => {
    const layout = withAgent();
    expect(shouldActOnAgentBrowserOpen(frame(), { layout, workspaceId: "w-2", visible: false })).toBe(true);
  });

  it("otherwise only a visible window on the same workspace acts", () => {
    const layout = openSession(defaultLayout(), "s-other");
    const act = (workspaceId: string | null, visible: boolean, open = frame()) =>
      shouldActOnAgentBrowserOpen(open, { layout, workspaceId, visible });
    expect(act("w-1", true)).toBe(true);
    expect(act("w-1", false)).toBe(false);
    expect(act("w-2", true)).toBe(false);
    expect(act(null, true, frame({ workspaceId: null }))).toBe(false);
  });
});

describe("placeAgentBrowser", () => {
  it("splits beside the session's pane and keeps focus where it was", () => {
    const l = withAgent();
    const agentPane = sessionPaneId(l, "s-agent")!;
    const next = placeAgentBrowser(l, frame({ path: "/app" }));
    expect(next.focusedPaneId).toBe(l.focusedPaneId);
    expect(panes(next.root)).toHaveLength(2);
    expect(activeTab(next, agentPane)).toEqual({ surface: "terminal", sessionId: "s-agent" });
    const [b] = browserTabs(next);
    expect(b.pane).not.toBe(agentPane);
    expect(b.tab).toMatchObject({ host: "localhost", port: 5173, path: "/app" });
    expect(activeTab(next, b.pane)).toBe(b.tab);
  });

  it("anchors on the session's pane even when another pane has focus", () => {
    let l = withAgent();
    l = splitPane(l, l.focusedPaneId, "row");
    l = openSession(l, "s-shell");
    const typing = l.focusedPaneId;
    const next = placeAgentBrowser(l, frame());
    expect(next.focusedPaneId).toBe(typing);
    expect(activeTab(next, typing)).toEqual({ surface: "terminal", sessionId: "s-shell" });
    expect(browserTabs(next)).toHaveLength(1);
  });

  it("re-points an existing pane on the same target instead of duplicating", () => {
    const first = placeAgentBrowser(withAgent(), frame({ path: "/a" }));
    const again = placeAgentBrowser(first, frame({ path: "/b?x=1" }));
    const tabs = browserTabs(again);
    expect(tabs).toHaveLength(1);
    expect(tabs[0].tab).toMatchObject({ path: "/b?x=1" });
    expect(panes(again.root)).toHaveLength(2);
    expect(again.focusedPaneId).toBe(first.focusedPaneId);
  });

  it("moves an existing tab out from behind the session rather than covering it", () => {
    let l = withAgent();
    l = openTab(l, freshBrowserTab("localhost", 5173, "/old"));
    const agentPane = sessionPaneId(l, "s-agent")!;
    l = activateTab(l, agentPane, 0); // the chat is showing; the browser hides behind it
    const next = placeAgentBrowser(l, frame({ path: "/new" }));
    expect(activeTab(next, agentPane)).toEqual({ surface: "terminal", sessionId: "s-agent" });
    const tabs = browserTabs(next);
    expect(tabs).toHaveLength(1);
    expect(tabs[0].pane).not.toBe(agentPane);
    expect(tabs[0].tab).toMatchObject({ path: "/new" });
    expect(next.focusedPaneId).toBe(agentPane);
  });

  it("collects a second app in the pane already showing a browser", () => {
    const one = placeAgentBrowser(withAgent(), frame({ port: 5173 }));
    const two = placeAgentBrowser(one, frame({ port: 6006 }));
    expect(panes(two.root)).toHaveLength(2);
    const tabs = browserTabs(two);
    expect(tabs).toHaveLength(2);
    expect(tabs[0].pane).toBe(tabs[1].pane);
    expect(two.focusedPaneId).toBe(one.focusedPaneId);
  });

  it("never splits past the pane cap", () => {
    let l = withAgent();
    while (panes(l.root).length < MAX_PANES) {
      l = splitPane(l, l.focusedPaneId, "col");
      l = openSession(l, `s-${panes(l.root).length}`);
    }
    const focused = l.focusedPaneId;
    const next = placeAgentBrowser(l, frame());
    expect(panes(next.root)).toHaveLength(MAX_PANES);
    expect(browserTabs(next)).toHaveLength(1);
    expect(browserTabs(next)[0].pane).not.toBe(sessionPaneId(next, "s-agent"));
    expect(next.focusedPaneId).toBe(focused);
  });

  it("without the session's tab, opens beside the focused pane", () => {
    const l = openSession(defaultLayout(), "s-other");
    const next = placeAgentBrowser(l, frame());
    expect(next.focusedPaneId).toBe(l.focusedPaneId);
    expect(activeTab(next, l.focusedPaneId)).toEqual({ surface: "terminal", sessionId: "s-other" });
    expect(browserTabs(next)).toHaveLength(1);
  });
});
