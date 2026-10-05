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
  setBrowserOpener,
  setBrowserPath,
  splitPane,
} from "../layout/layout";
import { sessionLabel, type Session } from "../workspace/sessions";

/** Who opened a browser pane, as the chrome shows it — read from the live
 *  roster each render, so a rename shows through and an ended session says
 *  so. `label` is null once the roster no longer has the session at all. */
export interface BrowserOpener {
  id: string;
  live: boolean;
  label: string | null;
  agentKind: string | null;
}

export function browserOpener(
  id: string,
  sessions: ReadonlyMap<string, Session>,
  names: ReadonlyMap<string, string>,
): BrowserOpener {
  const s = sessions.get(id);
  return {
    id,
    live: s?.alive === true,
    label: s === undefined ? null : sessionLabel(names, sessions, id),
    agentKind: s?.agent_kind ?? null,
  };
}

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
 *    panes collect in one place instead of splitting every time), else
 *    fills an empty pane,
 * 3. else splits beside the anchor (as a Cmd/Ctrl-clicked terminal URL
 *    does) while under the pane cap,
 * 4. else becomes a tab in the anchor's neighbour, or any other pane when
 *    the neighbour is the one the user is in.
 *
 * A zoomed pane stays zoomed: the pane opens behind it. The tab is credited
 * to the calling session (`openedBy`), so its chrome can say who opened it.
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
    // The latest opener wins: the pane now shows what THIS agent asked for.
    next = setBrowserOpener(setBrowserPath(next, existing.id, open.path), existing.id, open.sessionId);
    if (p.active === index || !covers(p.id)) {
      return restoreFocus(activateTab(next, p.id, index), l);
    }
    // Showing it in place would hide what the user is looking at: move it.
    next = detachTab(next, p.id, index);
    tab = { ...existing, path: open.path, openedBy: open.sessionId };
    break;
  }
  tab ??= freshBrowserTab(open.host, open.port, open.path, open.sessionId);

  const all = panes(next.root);
  // An empty pane shows nothing to cover — the anchor or the user's own
  // included — so it is taken before a split leaves it sitting empty.
  const home =
    all.find((p) => {
      const active = p.tabs[p.active];
      return !covers(p.id) && active !== undefined && active.surface === "browser";
    }) ?? all.find((p) => p.tabs.length === 0);
  if (home !== undefined) {
    next = openTab(focusPane(next, home.id), tab);
  } else if (panes(next.root).length < MAX_PANES) {
    next = openTab(splitPane(next, anchor, "row"), tab);
  } else {
    const neighbour = adjacentPane(next, anchor);
    const beside =
      neighbour !== null && !covers(neighbour)
        ? neighbour
        : (panes(next.root).find((p) => !covers(p.id))?.id ?? anchor);
    next = openTab(focusPane(next, beside), tab);
  }
  return restoreFocus(next, l);
}

/**
 * Hand focus back to the pane that had it, and its zoom with it (the layout
 * helpers used above clear zoom whenever focus moves).
 */
function restoreFocus(l: Layout, before: Layout): Layout {
  if (findPane(l.root, before.focusedPaneId) === null) return l;
  const focused = focusPane(l, before.focusedPaneId);
  return before.zoomedPaneId === before.focusedPaneId
    ? { ...focused, zoomedPaneId: before.zoomedPaneId }
    : focused;
}
