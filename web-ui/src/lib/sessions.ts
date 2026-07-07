import { api, ApiError } from "./api";

export interface Workspace {
  id: string;
  root: string;
  name: string;
  /** Unix seconds of the last open/activity; 0/absent on old daemons. */
  last_opened_at?: number;
}

export type SessionKind = "shell" | "agent";

/** Machine-readable attention state of an agent session (null for shells). */
export type AgentState =
  | "running"
  | "needs_permission"
  | "idle_prompt"
  | "finished"
  | "errored"
  | "rate_limited"
  | "unknown";

export interface Session {
  id: string;
  name: string;
  cwd: string;
  cols: number;
  rows: number;
  created_at: number;
  alive: boolean;
  exit_status: number | null;
  title: string | null;
  workspace_id: string;
  kind: SessionKind;
  agent_state: AgentState | null;
  /** Claude's own name for the conversation; overrides `name` for display. */
  agent_title: string | null;
  /**
   * Server-resolved display name (naming rule zero: the most specific thing
   * known about what the session is DOING). Optional until the daemon half
   * lands; the client falls back to agent_title/name.
   */
  display_name?: string | null;
  /**
   * The session's LIVE working directory (naming-v2 cwd poll), used by
   * drag-to-reference to relativize dropped paths. Optional until the daemon
   * half lands; the client falls back to the spawn cwd.
   */
  cwd_current?: string | null;
  /**
   * Files this agent wrote/edited (PostToolUse hooks, Write/Edit-class
   * tools): absolute paths, oldest first, deduped, capped server-side.
   * Null for shells; optional until the daemon half lands.
   */
  files_touched?: string[] | null;
}

/** The one display name for a session, used identically everywhere. */
export function displayName(s: Session): string {
  return s.display_name ?? s.agent_title ?? s.name;
}

/** True when the session is waiting on the user (drives the aggregate count). */
export function needsAttention(s: Session): boolean {
  return (
    s.agent_state === "needs_permission" ||
    s.agent_state === "idle_prompt" ||
    s.agent_state === "errored"
  );
}

/**
 * Dot modifier class for a session (shared by the rail, pane tabs, and the
 * session strip; see the global .dot.* styles in app.css).
 */
export function dotState(s: Session): string {
  if (s.kind !== "agent") return s.alive ? "alive" : "";
  switch (s.agent_state) {
    case "running":
      return "alive";
    case "needs_permission":
    case "idle_prompt":
      return "attn";
    case "finished":
      return "done";
    case "errored":
      return "err";
    case "rate_limited":
      return "rate";
    default:
      // "unknown" on a live process is a visible starting state (hollow
      // ring), distinct from both finished (filled) and dead (dim).
      return s.alive ? "starting" : "";
  }
}

/** Hover tooltip naming the state behind a session dot. */
export function dotTitle(s: Session): string {
  if (s.kind !== "agent") return s.alive ? "shell running" : "exited";
  switch (s.agent_state) {
    case "running":
      return "agent working";
    case "needs_permission":
      return "needs permission";
    case "idle_prompt":
      return "waiting for your input";
    case "finished":
      return "finished";
    case "errored":
      return "agent error";
    case "rate_limited":
      return "rate limited";
    default:
      return s.alive ? "starting…" : "exited";
  }
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
  if (res.status === 204) {
    return undefined as T;
  }
  return (await res.json()) as T;
}

export interface DirEntry {
  name: string;
  path: string;
}

export interface DirListing {
  path: string;
  parent: string | null;
  dirs: DirEntry[];
}

/** The daemon user's home directory. */
export async function fsHome(): Promise<string> {
  const body = await json<{ path: string }>(await api("/fs/home"));
  return body.path;
}

/**
 * List the subdirectories of `path`. The daemon expands a leading "~",
 * canonicalizes, and returns directories only; 400 with a message if the
 * path does not exist or is not a directory.
 */
export async function fsDirs(path: string, hidden = false): Promise<DirListing> {
  const q = new URLSearchParams({ path });
  if (hidden) q.set("hidden", "true");
  return json(await api(`/fs/dirs?${q.toString()}`));
}

export async function listWorkspaces(): Promise<Workspace[]> {
  return json(await api("/workspaces"));
}

export async function createWorkspace(root: string): Promise<Workspace> {
  return json(
    await api("/workspaces", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ root }),
    }),
  );
}

/** Stamp a workspace as freshly opened (home-screen recency). */
export async function touchWorkspace(id: string): Promise<Workspace> {
  return json(await api(`/workspaces/${id}/open`, { method: "POST" }));
}

/** Unregister a workspace from the daemon (the folder itself is untouched). */
export async function deleteWorkspace(id: string): Promise<void> {
  await json<void>(await api(`/workspaces/${id}`, { method: "DELETE" }));
}

export async function listSessions(): Promise<Session[]> {
  return json(await api("/sessions"));
}

export async function createSession(
  workspaceId: string,
  kind: SessionKind = "shell",
  name: string | null = null,
  size: { cols: number; rows: number } | null = null,
): Promise<Session> {
  // Spawn at the destination pane's fitted size so TUIs never boot at a
  // wrong 80x24 and repaint on the first resize (server clamps identically).
  const dims =
    size === null
      ? {}
      : {
          cols: Math.min(Math.max(Math.round(size.cols), 20), 500),
          rows: Math.min(Math.max(Math.round(size.rows), 5), 200),
        };
  return json(
    await api("/sessions", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ workspace_id: workspaceId, kind, name, ...dims }),
    }),
  );
}

export async function deleteSession(id: string): Promise<void> {
  await json<void>(await api(`/sessions/${id}`, { method: "DELETE" }));
}

/**
 * Poll GET /api/v1/sessions on an interval to refresh names/titles/alive.
 * Fires immediately, then every `intervalMs`. Returns a stop function.
 */
export function pollSessions(
  onResult: (sessions: Session[]) => void,
  onError: (e: unknown) => void,
  intervalMs = 5000,
): () => void {
  let stopped = false;
  const tick = async () => {
    try {
      const list = await listSessions();
      if (!stopped) onResult(list);
    } catch (e) {
      if (!stopped) onError(e);
    }
  };
  void tick();
  const id = setInterval(tick, intervalMs);
  return () => {
    stopped = true;
    clearInterval(id);
  };
}
