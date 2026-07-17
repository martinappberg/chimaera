/**
 * Shared types + pure helpers for the workspace dashboard surface.
 *
 * `DashCtx` is the one prop App threads through SplitNode → Pane into
 * DashboardView: the app-level data and actions the dashboard needs that the
 * pane tree doesn't already carry (recents, MRU, the new-session actions).
 * Everything session-shaped rides the existing `sessions` map prop instead.
 */

import type { RecentConvo } from "../workspace/launcher";
import type { Session } from "../workspace/sessions";
import { agentKind } from "../workspace/sessions";

export interface DashCtx {
  /** Active workspace display name (the folder basename). */
  wsName: string;
  /** The first session snapshot arrived — render data, not the skeleton.
   *  (Mid ledger-restore a roster read would show "everything died".) */
  ready: boolean;
  /** Ended conversations for this workspace, newest first (the rail's set). */
  recents: RecentConvo[];
  /** Agent session ids this window focused, most recent first. */
  mru: string[];
  /** The workspace's Mastermind binding (the additive `GET /workspaces`
   *  field); null when none is configured. The dock renders from this. */
  mastermind: { session_id: string; mode: "ask" | "auto"; agent?: string } | null;
  /** Re-fetch the workspaces list — the Mastermind binding lives there, so
   *  the dock calls this after a PUT/DELETE. Resolves once applied. */
  refreshWorkspaces: () => Promise<void>;
  onOpenRecent: (r: RecentConvo) => void;
  onNewTerminal: () => void;
  onNewAgent: () => void;
  onOpenGit: () => void;
  /** Open/focus a session tab (rail-click semantics). */
  onOpenSession: (id: string) => void;
}

/**
 * How much the dashboard can truthfully know about a session — the honesty
 * axis every card wears openly. Chat sessions are protocol-observed
 * (authoritative); claude TUIs report through hooks (coarse); other TUIs
 * have no integration, so their state is honestly unknown.
 */
export type Provenance = "protocol" | "hooks" | "none";

export function provenanceOf(s: Session): Provenance {
  if (s.ui === "chat") return "protocol";
  return agentKind(s) === "claude" ? "hooks" : "none";
}

export function provenanceTitle(p: Provenance): string {
  switch (p) {
    case "protocol":
      return "status from the chat protocol — authoritative";
    case "hooks":
      return "status from Claude Code hooks — coarse but honest";
    default:
      return "no status integration — the process runs, but its state is unknown";
  }
}

/**
 * Roster sort weight: live sessions first, dead ones last — deliberately NOT
 * re-ranked per activity state. A chat agent cycles running↔finished every
 * turn, and a roster that reshuffles on each turn end reads as chaos; the
 * state dot, the strip sentence, and the attention lane already say who is
 * working and who needs you. Within a bucket the order is stable (created_at,
 * applied by the caller).
 */
export function rosterWeight(s: Session): number {
  return s.alive ? 0 : 1;
}

/** Workspace-relative display path (verbatim when outside the root). */
export function relPath(wsRoot: string | null, p: string): string {
  return wsRoot !== null && p.startsWith(`${wsRoot}/`) ? p.slice(wsRoot.length + 1) : p;
}
