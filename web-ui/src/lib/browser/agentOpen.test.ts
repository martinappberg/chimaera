import { describe, expect, it } from "vitest";
import {
  type Layout,
  MAX_PANES,
  activateTab,
  defaultLayout,
  findPane,
  focusPane,
  freshBrowserTab,
  openSession,
  openTab,
  panes,
  deserializeLayout,
  serializeLayout,
  sessionPaneId,
  setBrowserPath,
  setBrowserTarget,
  splitPane,
  toggleZoom,
} from "../layout/layout";
import type { Session } from "../workspace/sessions";
import {
  type AgentBrowserOpen,
  browserOpener,
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

  it("at the pane cap, never covers the pane the user is in", () => {
    let l = withAgent();
    const agentPane = l.focusedPaneId;
    while (panes(l.root).length < MAX_PANES) {
      l = splitPane(l, agentPane, "row");
      l = openSession(l, `s-${panes(l.root).length}`);
    }
    // The user is in each other pane in turn, the agent's neighbour included.
    for (const p of panes(l.root)) {
      if (p.id === agentPane) continue;
      const before = focusPane(l, p.id);
      const next = placeAgentBrowser(before, frame());
      expect(next.focusedPaneId).toBe(p.id);
      expect(activeTab(next, p.id)).toEqual(activeTab(before, p.id));
      expect(activeTab(next, agentPane)).toEqual(activeTab(before, agentPane));
      expect(browserTabs(next)).toHaveLength(1);
    }
  });

  it("keeps a zoomed pane zoomed", () => {
    const base = withAgent();
    const l = toggleZoom(splitPane(base, base.focusedPaneId, "row"));
    expect(panes(l.root)).toHaveLength(2);
    expect(l.zoomedPaneId).toBe(l.focusedPaneId);
    const next = placeAgentBrowser(l, frame());
    expect(next.zoomedPaneId).toBe(l.zoomedPaneId);
    expect(next.focusedPaneId).toBe(l.focusedPaneId);
    expect(browserTabs(next)).toHaveLength(1);
  });

  it("fills an empty pane instead of splitting beside it", () => {
    // A window on the workspace with nothing open: the one empty pane.
    const blank = defaultLayout();
    const solo = placeAgentBrowser(blank, frame());
    expect(panes(solo.root)).toHaveLength(1);
    expect(browserTabs(solo)).toHaveLength(1);
    expect(solo.focusedPaneId).toBe(blank.focusedPaneId);

    // An empty pane beside the agent is used; no third pane appears.
    const base = withAgent();
    const agentPane = sessionPaneId(base, "s-agent")!;
    const l = focusPane(splitPane(base, agentPane, "row"), agentPane);
    const next = placeAgentBrowser(l, frame());
    expect(panes(next.root)).toHaveLength(2);
    expect(browserTabs(next)[0].pane).not.toBe(agentPane);
    expect(next.focusedPaneId).toBe(agentPane);
  });

  it("without the session's tab, opens beside the focused pane", () => {
    const l = openSession(defaultLayout(), "s-other");
    const next = placeAgentBrowser(l, frame());
    expect(next.focusedPaneId).toBe(l.focusedPaneId);
    expect(activeTab(next, l.focusedPaneId)).toEqual({ surface: "terminal", sessionId: "s-other" });
    expect(browserTabs(next)).toHaveLength(1);
  });
});

describe("who opened a browser pane", () => {
  const opener = (l: Layout) =>
    browserTabs(l).map(({ tab }) => (tab.surface === "browser" ? tab.openedBy : null));

  it("credits a new pane to the calling session, and a re-point to the latest opener", () => {
    const first = placeAgentBrowser(withAgent(), frame({ path: "/a" }));
    expect(opener(first)).toEqual(["s-agent"]);
    const again = placeAgentBrowser(first, frame({ sessionId: "s-other", path: "/b" }));
    expect(opener(again)).toEqual(["s-other"]);
  });

  it("credits a tab moved out from behind the session", () => {
    let l = withAgent();
    l = openTab(l, freshBrowserTab("localhost", 5173, "/old"));
    l = activateTab(l, sessionPaneId(l, "s-agent")!, 0);
    expect(opener(l)).toEqual([undefined]);
    expect(opener(placeAgentBrowser(l, frame()))).toEqual(["s-agent"]);
  });

  it("a pane the user opened has no opener", () => {
    expect(Object.keys(freshBrowserTab("localhost", 1, "/"))).not.toContain("openedBy");
  });

  it("the user pointing it elsewhere drops the attribution; the node hunt and in-app paths keep it", () => {
    const l = placeAgentBrowser(withAgent(), frame());
    const id = browserTabs(l)[0].tab.id;
    expect(opener(setBrowserPath(l, id, "/elsewhere"))).toEqual(["s-agent"]);
    expect(opener(setBrowserTarget(l, id, "localhost", 5173, "/x"))).toEqual(["s-agent"]);
    expect(opener(setBrowserTarget(l, id, "node-7", 5173, "/", true))).toEqual(["s-agent"]);
    expect(opener(setBrowserTarget(l, id, "localhost", 9999, "/"))).toEqual([undefined]);
  });

  it("round-trips through the saved layout", () => {
    const l = placeAgentBrowser(withAgent(), frame({ path: "/app" }));
    const back = deserializeLayout(JSON.parse(JSON.stringify(serializeLayout(l))));
    expect(back).not.toBeNull();
    expect(browserTabs(back!).map(({ tab }) => tab)).toEqual(browserTabs(l).map(({ tab }) => tab));
  });

  it("restores an older layout without the field unchanged, and drops a garbage value", () => {
    const saved = (wb?: unknown) => ({
      v: 1,
      focusMode: false,
      zoom: null,
      focused: "p1",
      root: {
        t: "p",
        id: "p1",
        tabs: [
          { s: "s-agent" },
          { w: "localhost", wo: 5173, wi: "b1", wp: "/", ...(wb === undefined ? {} : { wb }) },
        ],
        active: 0,
      },
    });
    expect(browserTabs(deserializeLayout(saved())!).map(({ tab }) => tab)).toEqual([
      { surface: "browser", id: "b1", host: "localhost", port: 5173, path: "/" },
    ]);
    for (const wb of [42, "", "s-<script>", "x".repeat(65), { id: "s-1" }]) {
      const l = deserializeLayout(saved(wb));
      expect(l).not.toBeNull();
      expect(panes(l!.root)[0].tabs).toHaveLength(2);
      expect(opener(l!)).toEqual([undefined]);
    }
    expect(opener(deserializeLayout(saved("s-1a2b3c4d"))!)).toEqual(["s-1a2b3c4d"]);
  });

  it("names the opener from the live roster, and says when it has ended", () => {
    const s = (over: Record<string, unknown>) =>
      ({ id: "s-1", name: "fix CI", kind: "agent", agent_kind: "codex", alive: true, ...over }) as unknown as Session;
    const roster = new Map([["s-1", s({})]]);
    expect(browserOpener("s-1", roster, new Map())).toEqual({
      id: "s-1",
      live: true,
      label: "fix CI",
      agentKind: "codex",
    });
    // A pinned rename shows through.
    expect(browserOpener("s-1", roster, new Map([["s-1", "frontend"]])).label).toBe("frontend");
    expect(browserOpener("s-1", new Map([["s-1", s({ alive: false })]]), new Map()).live).toBe(false);
    expect(browserOpener("s-gone", roster, new Map())).toEqual({
      id: "s-gone",
      live: false,
      label: null,
      agentKind: null,
    });
  });
});
