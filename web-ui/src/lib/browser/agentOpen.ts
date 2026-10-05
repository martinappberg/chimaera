/**
 * Agent-opened browser panes: the window half of the MCP `open_browser`
 * tool. The daemon validated the address and applied the proxy's mint
 * allowlist, then pushed a `browser_open` frame on `/ws/events` to every
 * connected window; each window decides here whether to act and where the
 * pane goes. The pane itself is an ordinary `BrowserTab` — it mints its
 * proxy ticket on mount like any other.
 *
 * Pure (no DOM, no stores) so the decisions are unit-tested.
 */

import {
  type Layout,
  type Tab,
  MAX_PANES,
  activateTab,
  adjacentPane,
  detachTab,
  findPane,
  focusPane,
  freshBrowserTab,
  openTab,
  panes,
  sessionPaneId,
  setBrowserPath,
  splitPane,
} from "../layout/layout";

/** One `{"type":"browser_open", ...}` frame, validated. */
export interface AgentBrowserOpen {
  /** The agent session that asked. */
  sessionId: string;
  /** That session's workspace (null when it has none). */
  workspaceId: string | null;
  host: string;
  port: number;
  /** Path + query + fragment, normalized like a clicked URL's. */
  path: string;
}

/** Validate a frame; null for anything malformed (dropped, never thrown). */
export function parseAgentBrowserOpen(raw: unknown): AgentBrowserOpen | null {
  if (typeof raw !== "object" || raw === null) return null;
  const f = raw as Record<string, unknown>;
  const { session_id, workspace_id, host, port, path } = f;
  if (typeof session_id !== "string" || session_id === "") return null;
  if (typeof host !== "string" || host === "") return null;
  if (typeof port !== "number" || !Number.isInteger(port) || port <= 0 || port > 65535) return null;
  if (typeof path !== "string" || !path.startsWith("/")) return null;
  // The same normalization a terminal URL click gets (`proxyableUrl` reads
  // a parsed URL): dot segments resolve and unsafe characters encode, so
  // the iframe address can never climb out of `/proxy/{id}`. Prefixing the
  // origin keeps a leading `//` a path, never an authority.
  let normalized: string;
  try {
    const u = new URL(`http://h${path}`);
    normalized = `${u.pathname}${u.search}${u.hash}`;
  } catch {
    return null;
  }
  return {
    sessionId: session_id,
    workspaceId: typeof workspace_id === "string" && workspace_id !== "" ? workspace_id : null,
    host,
    port,
    path: normalized,
  };
}

/** What a window knows about itself when a frame arrives. */
export interface WindowView {
  layout: Layout;
  /** The workspace this window shows. */
  workspaceId: string | null;
  /** The document is visible (not minimized / a hidden tab). */
  visible: boolean;
}

/**
 * Does THIS window open the pane? The window holding the calling session's
 * tab always does; a window without it only when it shows the same
 * workspace and someone can see it. Several such windows may all act —
 * acceptable, since none of them holds the session.
 */
export function shouldActOnAgentBrowserOpen(open: AgentBrowserOpen, view: WindowView): boolean {
  if (sessionPaneId(view.layout, open.sessionId) !== null) return true;
  return view.visible && open.workspaceId !== null && open.workspaceId === view.workspaceId;
}

/**
 * Put the pane in front of the user WITHOUT moving their focus: the pane
 * they are typing in stays the focused one, and what it shows is never
 * covered. Anchored on the calling session's pane (else the focused one):
 *
 * 1. A tab already on the same host:port is re-pointed at the new path and
 *    shown — moved beside the anchor if showing it would cover the anchor's
 *    or the focused pane's current view.
 * 2. Otherwise it joins a pane that is already showing a browser (agent
 *    panes collect in one place instead of splitting every time),
 * 3. else splits beside the anchor (as a Cmd/Ctrl-clicked terminal URL
 *    does) while under the pane cap,
 * 4. else becomes a tab in the anchor's neighbour.
 */
export function placeAgentBrowser(l: Layout, open: AgentBrowserOpen): Layout {
  const focused = l.focusedPaneId;
  const anchor = sessionPaneId(l, open.sessionId) ?? focused;
  const covers = (paneId: string) => paneId === anchor || paneId === focused;

  let next = l;
  let tab: Tab | null = null;
  for (const p of panes(l.root)) {
    const index = p.tabs.findIndex(
      (t) => t.surface === "browser" && t.host === open.host && t.port === open.port,
    );
    if (index < 0) continue;
    const existing = p.tabs[index];
    if (existing.surface !== "browser") break;
    next = setBrowserPath(next, existing.id, open.path);
    if (p.active === index || !covers(p.id)) {
      return restoreFocus(activateTab(next, p.id, index), focused);
    }
    // Showing it in place would hide what the user is looking at: move it.
    next = detachTab(next, p.id, index);
    tab = { ...existing, path: open.path };
    break;
  }
  tab ??= freshBrowserTab(open.host, open.port, open.path);

  const browserPane = panes(next.root).find((p) => {
    const active = p.tabs[p.active];
    return !covers(p.id) && active !== undefined && active.surface === "browser";
  });
  if (browserPane !== undefined) {
    next = openTab(focusPane(next, browserPane.id), tab);
  } else if (panes(next.root).length < MAX_PANES) {
    next = openTab(splitPane(next, anchor, "row"), tab);
  } else {
    const beside = adjacentPane(next, anchor) ?? anchor;
    next = openTab(focusPane(next, beside), tab);
  }
  return restoreFocus(next, focused);
}

/** Hand focus back to the pane that had it (if it still exists). */
function restoreFocus(l: Layout, paneId: string): Layout {
  return findPane(l.root, paneId) !== null ? focusPane(l, paneId) : l;
}
