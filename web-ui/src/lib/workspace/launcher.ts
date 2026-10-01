/**
 * The agent launcher's client side (DESIGN.md "The agent launcher"):
 * catalog + resumables fetch, and the persisted default config that the
 * split button / Cmd+Shift+E spawn instantly.
 */

import { writable } from "svelte/store";
import { api, ApiError } from "../net/api";

/** One agent CLI as reported by GET /api/v1/agents. */
export interface AgentInfo {
  id: string;
  name: string;
  installed: boolean;
  /** Installed but too old to run usefully — offer an in-app update
   *  instead of spawning blind (npm-era codex, pre-login). */
  outdated: boolean;
  /** `--version` first line for installed agents, when the probe worked. */
  version: string | null;
  /** The resolved binary lives under ~/.chimaera/agents — installed (or
   *  updated) by chimaera's managed-runtime flow, not the user's own PATH. */
  managed: boolean;
  /** The resolved binary came from the user's explicit `agents.<id>.path`
   *  setting (a runnable one) — provenance for the Agents settings panel. */
  explicit: boolean;
  /** The resolved binary's absolute path (which `{id}` a spawn will run);
   *  null when not installed. Surfaced in the launcher's version tooltip so
   *  "yours" vs "chimaera" is answerable at a glance. */
  path: string | null;
  /** Whether POST /agents/{id}/install has a curated managed install.
   *  False means the POST would 400 —
   *  no install chip; the docs link is the affordance. */
  managedInstall: boolean;
  /** Official docs URL — a clickable link on every launcher row. */
  installUrl: string | null;
  /** Curated model choices for this agent (`--model` ids + labels). */
  models: { id: string; label: string }[];
  /** Whether this agent can run as a structured chat session. */
  chatCapable: boolean;
  chatSetupRequired?: boolean;
  forkCapable?: boolean;
  /** The newest upstream release the daemon knows of (bare number), from
   *  its slow periodic probe or a `check=true` re-check. Null until a
   *  probe has landed. */
  latestVersion: string | null;
  /** When that probe ran, unix seconds. */
  latestCheckedAt: number | null;
  latestError?: string | null;
  installation?: { sessionId: string; running: boolean; exitStatus: number | null };
  /** Installed and strictly older than `latestVersion` — the update
   *  affordance: one-click for a managed binary (POST update), quiet
   *  information for the user's own. Never guessed: unparseable versions
   *  stay false. */
  updateAvailable: boolean;
}


async function json<T>(res: Response): Promise<T> {
  if (!res.ok) {
    let message = `request failed with status ${res.status}`;
    try {
      const body = (await res.json()) as { error?: string };
      if (body.error) message = body.error;
    } catch {
      // non-JSON error body; keep the generic message
    }
    throw new ApiError(res.status, message);
  }
  return (await res.json()) as T;
}

/** Memoized catalog: repeated ChatView mounts (and the launcher) share ONE
 *  in-flight/resolved fetch instead of re-hitting GET /agents each time, which
 *  reflashed the header model chip. `refresh` bypasses it and replaces it (App
 *  re-probes after the install flow); a rejected fetch is dropped so a transient
 *  error can't poison later calls. */
const catalog = writable<AgentInfo[]>([]);
export const agentCatalog = { subscribe: catalog.subscribe };

let agentsCache: Promise<AgentInfo[]> | null = null;

/** Ready chats first; setup never looks like a launch action. Stable sorting
 * preserves the curated/provider order within each group. */
export function orderAgents(agents: AgentInfo[]): AgentInfo[] {
  const rank = (a: AgentInfo) => a.installed && !a.outdated && a.chatCapable ? 0 : 1;
  return [...agents].sort((a, b) => rank(a) - rank(b));
}

/** GET /api/v1/agents — what this host has, per known agent. `check` also
 *  probes upstream for each agent's latest release inline (Settings'
 *  re-check); the launcher never passes it — its rows must paint instantly,
 *  and the daemon's slow probe keeps the cached answer fresh enough. */
export function listAgents(refresh = false, check = false): Promise<AgentInfo[]> {
  if (!refresh && !check && agentsCache !== null) return agentsCache;
  const pending = fetchAgents(refresh, check);
  agentsCache = pending;
  void pending.catch(() => {
    if (agentsCache === pending) agentsCache = null;
  });
  return pending;
}

/** Re-fetch the catalog WITHOUT `refresh` (poll cadence): bypasses this
 *  module's memo but not the daemon's detection cache — the daemon's install
 *  watcher already re-detects when an install/update session ends, and its
 *  mtime validation notices a swapped binary, so a poll never needs to force
 *  four login-shell re-resolutions per tick. Replaces the memoized catalog. */
export function pollAgents(signal?: AbortSignal): Promise<AgentInfo[]> {
  const pending = fetchAgents(false, false, signal);
  agentsCache = pending;
  void pending.catch(() => {
    if (agentsCache === pending) agentsCache = null;
  });
  return pending;
}

async function fetchAgents(refresh: boolean, check = false, signal?: AbortSignal): Promise<AgentInfo[]> {
  const params = new URLSearchParams();
  if (refresh) params.set("refresh", "true");
  if (check) params.set("check", "true");
  // Not URLSearchParams.size: it's missing on older WebKit (Safari < 17 /
  // webkit2gtk), where `undefined > 0` would silently drop the params.
  const qs = params.toString();
  const body = await json<unknown>(await api(`/agents${qs === "" ? "" : `?${qs}`}`, { signal }));
  const rows = normalizeAgentCatalog(body);
  catalog.set(rows);
  return rows;
}

/** Catalog identities are data, including future namespaced extension agents.
 * Reject malformed/duplicate rows before they reach keyed menus. */
export function normalizeAgentCatalog(body: unknown): AgentInfo[] {
  if (!Array.isArray(body)) return [];
  const ids = new Set<string>();
  return body.slice(0, 64).flatMap((raw): AgentInfo[] => {
    if (typeof raw !== "object" || raw === null) return [];
    const a = raw as Record<string, unknown>;
    if (typeof a.id !== "string" || !a.id || a.id.length > 128 || ids.has(a.id)) return [];
    ids.add(a.id);
    const modelIds = new Set<string>();
    const install =
      typeof a.install === "object" && a.install !== null
        ? (a.install as Record<string, unknown>)
        : {};
    const operation = typeof a.installation === "object" && a.installation !== null
      ? a.installation as Record<string, unknown> : null;
    return [
      {
        installation: operation && typeof operation.session_id === "string" ? {
          sessionId: operation.session_id, running: operation.running === true,
          exitStatus: typeof operation.exit_status === "number" ? operation.exit_status : null,
        } : undefined,
        id: a.id,
        name: typeof a.name === "string" ? a.name : a.id,
        installed: a.installed === true,
        outdated: a.outdated === true,
        version: typeof a.version === "string" ? a.version : null,
        managed: a.managed === true,
        explicit: a.explicit === true,
        path: typeof a.path === "string" ? a.path : null,
        managedInstall: a.managed_install === true,
        installUrl: typeof install.url === "string" ? install.url : null,
        models: Array.isArray(a.models)
          ? a.models.slice(0, 64).flatMap((m): { id: string; label: string }[] => {
              if (typeof m !== "object" || m === null) return [];
              const mm = m as Record<string, unknown>;
              if (typeof mm.id !== "string" || !mm.id || mm.id.length > 128 || modelIds.has(mm.id) || typeof mm.label !== "string") return [];
              modelIds.add(mm.id);
              return [{ id: mm.id, label: mm.label.slice(0, 256) }];
            })
          : [],
        chatCapable: a.chat_capable === true,
        chatSetupRequired: a.chat_setup_required === true,
        forkCapable: a.fork_capable === true || (a.fork_capable === undefined && (a.id === "claude" || a.id === "codex")),
        latestVersion: typeof a.latest_version === "string" ? a.latest_version : null,
        latestError: typeof a.latest_error === "string" ? a.latest_error : null,
        latestCheckedAt: typeof a.latest_checked_at === "number" ? a.latest_checked_at : null,
        updateAvailable: a.update_available === true,
      },
    ];
  });
}

/**
 * POST /api/v1/agents/{id}/install — managed runtimes: the daemon builds
 * the CURATED install/update command itself (official artifacts only, into
 * ~/.chimaera/agents) and spawns it as an ordinary shell session in
 * `workspaceId`, so the installer output streams into a normal pane.
 * Returns the spawned session id; 409 when an install for that agent is
 * already running.
 */
export async function installAgent(agentId: string, workspaceId: string): Promise<string> {
  const body = await json<{ session_id?: unknown }>(
    await api(`/agents/${encodeURIComponent(agentId)}/install`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ workspace_id: workspaceId }),
    }),
  );
  if (typeof body.session_id !== "string") {
    throw new ApiError(500, "malformed install response");
  }
  return body.session_id;
}

/**
 * POST /api/v1/agents/{id}/update — bring a chimaera-MANAGED binary to the
 * latest release: the daemon re-runs its curated script (fetch latest +
 * atomic symlink re-swap) as a visible "update <agent>" session in
 * `workspaceId`. Managed only — the daemon 400s for a personal binary, and
 * the UI never offers the action for one. Returns the spawned session id;
 * 409 while an install/update for that agent is already running.
 */
export async function updateAgent(agentId: string, workspaceId: string): Promise<string> {
  const body = await json<{ session_id?: unknown }>(
    await api(`/agents/${encodeURIComponent(agentId)}/update`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ workspace_id: workspaceId }),
    }),
  );
  if (typeof body.session_id !== "string") {
    throw new ApiError(500, "malformed update response");
  }
  return body.session_id;
}

/**
 * DELETE /api/v1/agents/{id}/install — uninstall a chimaera-MANAGED agent
 * binary (the symlink + version tree under ~/.chimaera/agents). Only touches
 * chimaera's own prefix; a user's own install is never affected. Returns
 * whether anything managed was actually removed.
 */
export async function uninstallAgent(agentId: string): Promise<boolean> {
  const body = await json<{ removed?: unknown }>(
    await api(`/agents/${encodeURIComponent(agentId)}/install`, { method: "DELETE" }),
  );
  return body.removed === true;
}

/** A launcher selection: what to spawn (and, for resume rows, from where). */
export interface LaunchPick {
  agent: string;
  resume?: string;
  /** Which surface the user explicitly chose in the launcher — "open" (chat)
   *  vs the terminal button. Omitted = follow the agents.defaultView setting. */
  ui?: "chat" | "term";
  /** True when the user picked the surface deliberately (the "open"/terminal
   *  buttons, ⌘↵) — that choice becomes the sticky default. A plain row press
   *  is NOT explicit: it follows the current default without changing it. */
  explicit?: boolean;
}

// --- the rail Recents section ------------------------------------------------

/** One ended agent conversation from GET /api/v1/recents (newest first). */
export interface RecentConvo {
  /** The identity an archive hides it under (its native id, or a
   *  kind+title stand-in for a handle-less row). */
  key: string;
  /** Which agent CLI ran it ("claude"/"codex"/"gemini") — drives the glyph. */
  kind: string;
  title: string;
  /** Native conversation handle (Claude session id / Codex thread id);
   *  null = clicking starts a fresh session. */
  resume: string | null;
  /** When the session ended, unix seconds. */
  lastActive: number;
  /** The surface it last ran on ("chat"/"term"), so reopening the row lands in
   *  the same mode. Null for pre-`ui` entries and scanned transcripts → the
   *  conversation reopens in terminal view. */
  ui: "chat" | "term" | null;
}

/** GET /api/v1/recents — the workspace's ended agent conversations. */
export async function listRecents(workspaceId: string): Promise<RecentConvo[]> {
  const q = new URLSearchParams({ workspace_id: workspaceId });
  const body = await json<unknown>(await api(`/recents?${q.toString()}`));
  if (!Array.isArray(body)) return [];
  return body.flatMap((raw): RecentConvo[] => {
    if (typeof raw !== "object" || raw === null) return [];
    const r = raw as Record<string, unknown>;
    if (typeof r.kind !== "string" || typeof r.title !== "string") return [];
    const resume = typeof r.resume === "string" ? r.resume : null;
    return [
      {
        key:
          typeof r.key === "string" && r.key !== ""
            ? r.key
            : (resume ?? `~${r.kind}:${r.title}`),
        kind: r.kind,
        title: r.title,
        resume,
        lastActive: typeof r.last_active === "number" ? r.last_active : 0,
        ui: r.ui === "chat" || r.ui === "term" ? r.ui : null,
      },
    ];
  });
}

// --- the persisted default agent ---------------------------------------------

/** What the split button's main surface spawns: latest chosen, persisted. */
export interface AgentDefault {
  agent: string;
}

const DEFAULT_KEY = "chimaera.agentDefault";

/** The persisted default agent; falls back to claude. */
export function getAgentDefault(): AgentDefault {
  try {
    const raw = localStorage.getItem(DEFAULT_KEY);
    if (raw !== null) {
      const v = JSON.parse(raw) as { agent?: unknown };
      if (typeof v.agent === "string" && v.agent !== "") {
        return { agent: v.agent };
      }
    }
  } catch {
    // corrupted blob; fall through to the built-in default
  }
  return { agent: "claude" };
}

/** Persist the default agent (every launcher selection becomes it). */
export function setAgentDefault(d: AgentDefault): void {
  try {
    localStorage.setItem(DEFAULT_KEY, JSON.stringify(d));
  } catch {
    // storage unavailable (private mode); the default just doesn't stick
  }
}

/** Compact relative age for resume rows ("now", "5m", "3h", "2d"). */
/** The bare version number, wherever the CLI buried it ("codex-cli 0.52.0",
 *  "2.1.202 (Claude Code)"). Shared by the launcher and both settings panels. */
export function versionNumber(version: string): string {
  return version.split(" ").find((t) => /^\d/.test(t)) ?? version.split(" ")[0];
}

export function relativeAge(mtimeSecs: number, nowMs = Date.now()): string {
  const s = Math.max(0, Math.floor(nowMs / 1000) - mtimeSecs);
  if (s < 60) return "now";
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h`;
  const d = Math.floor(h / 24);
  if (d < 30) return `${d}d`;
  const mo = Math.floor(d / 30);
  if (mo < 12) return `${mo}mo`;
  return `${Math.floor(d / 365)}y`;
}
