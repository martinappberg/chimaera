/**
 * The agent launcher's client side (DESIGN.md "The agent launcher"):
 * catalog + resumables fetch, and the persisted default config that the
 * split button / Cmd+Shift+E spawn instantly.
 */

import { api, ApiError } from "./api";

/** One agent CLI as reported by GET /api/v1/agents. */
export interface AgentInfo {
  id: string;
  name: string;
  installed: boolean;
  /** Installed but too old to run usefully — offer the install command as
   *  an update instead of spawning blind (npm-era codex, pre-login). */
  outdated: boolean;
  /** `--version` first line for installed agents, when the probe worked. */
  version: string | null;
  /** Install command the UI pre-types (never executes) into a fresh terminal. */
  install: string;
  /** Official docs URL — a clickable link on every launcher row. */
  installUrl: string | null;
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

/** GET /api/v1/agents — what this host has, per known agent. */
export async function listAgents(refresh = false): Promise<AgentInfo[]> {
  const body = await json<unknown>(await api(`/agents${refresh ? "?refresh=true" : ""}`));
  if (!Array.isArray(body)) return [];
  return body.flatMap((raw): AgentInfo[] => {
    if (typeof raw !== "object" || raw === null) return [];
    const a = raw as Record<string, unknown>;
    if (typeof a.id !== "string") return [];
    const install =
      typeof a.install === "object" && a.install !== null
        ? (a.install as Record<string, unknown>)
        : {};
    return [
      {
        id: a.id,
        name: typeof a.name === "string" ? a.name : a.id,
        installed: a.installed === true,
        outdated: a.outdated === true,
        version: typeof a.version === "string" ? a.version : null,
        install: typeof install.command === "string" ? install.command : "",
        installUrl: typeof install.url === "string" ? install.url : null,
      },
    ];
  });
}

/** A launcher selection: what to spawn (and, for resume rows, from where). */
export interface LaunchPick {
  agent: string;
  resume?: string;
}

// --- the rail Recents section ------------------------------------------------

/** One ended agent conversation from GET /api/v1/recents (newest first). */
export interface RecentConvo {
  /** Which agent CLI ran it ("claude"/"codex"/"gemini") — drives the glyph. */
  kind: string;
  title: string;
  /** Claude session id to resume; null = clicking starts a fresh one. */
  resume: string | null;
  /** When the session ended, unix seconds. */
  lastActive: number;
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
    return [
      {
        kind: r.kind,
        title: r.title,
        resume: typeof r.resume === "string" ? r.resume : null,
        lastActive: typeof r.last_active === "number" ? r.last_active : 0,
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
