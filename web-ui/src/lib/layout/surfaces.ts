/**
 * The window's "surface manifest": a normalized, capped list of what this
 * window shows, derived from the layout tree and sent as an ADDITIVE
 * `surfaces` key on the per-window view-state PUT (design §8 — the
 * Mastermind's "open windows" sense). Never file contents, only identities:
 * paths (workspace-relative when under the root), session ids, and the
 * singleton surface names. Exactly one entry may carry `focused: true` —
 * the active tab of the focused pane.
 */
import { findPane, panes, type Layout, type Tab } from "./layout";

export interface SurfaceRef {
  surface: string;
  /** Files, Finders and diffs: workspace-relative when under `wsRoot`. */
  path?: string;
  /** Sessions and session-scoped changes reviews. */
  sid?: string;
  focused?: true;
}

export const SURFACES_CAP = 40;

function rel(wsRoot: string | null, p: string): string {
  return wsRoot !== null && p.startsWith(`${wsRoot}/`) ? p.slice(wsRoot.length + 1) : p;
}

function refOf(t: Tab, wsRoot: string | null): SurfaceRef {
  switch (t.surface) {
    case "terminal":
      return { surface: "session", sid: t.sessionId };
    case "file":
      return { surface: "file", path: rel(wsRoot, t.path) };
    case "finder":
      return { surface: "finder", path: rel(wsRoot, t.path) };
    case "diff":
      return { surface: "diff", path: rel(wsRoot, t.path) };
    case "changes":
      return { surface: "changes", sid: t.sessionId };
    default:
      // dashboard · knowledge · timeline · plugins · git · settings · browser
      return { surface: t.surface };
  }
}

/** Tree order (pane by pane, tab by tab), the focused pane's active tab
 *  flagged, capped at SURFACES_CAP — the focused entry always survives the cap. */
export function surfacesOf(l: Layout, wsRoot: string | null, cap = SURFACES_CAP): SurfaceRef[] {
  const focusedPane = findPane(l.root, l.focusedPaneId);
  const focusedTab = focusedPane?.tabs[focusedPane.active] ?? null;
  const out: SurfaceRef[] = [];
  let focusedRef: SurfaceRef | null = null;
  for (const p of panes(l.root)) {
    for (const t of p.tabs) {
      const ref = refOf(t, wsRoot);
      if (p === focusedPane && t === focusedTab) {
        ref.focused = true;
        focusedRef = ref;
      }
      out.push(ref);
    }
  }
  if (out.length <= cap) return out;
  const head = out.slice(0, cap);
  if (focusedRef !== null && !head.includes(focusedRef)) head[cap - 1] = focusedRef;
  return head;
}
