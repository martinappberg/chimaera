import { writable } from "svelte/store";

import { api, ApiError } from "./api";
import { getSetting, resolvedTheme } from "./settings/store.svelte";

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
  /**
   * Which agent CLI a kind-"agent" session runs ("claude", "codex",
   * "gemini"); drives the glyph choice everywhere sessions appear. Null for
   * shells; optional (treated as "claude") until the daemon half lands.
   */
  agent_kind?: string | null;
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
  /**
   * Shell phase from OSC 133 marks: at the prompt ("ready"), running a
   * command ("running"), or no integration seen ("unknown"). Shells only.
   */
  phase?: "unknown" | "ready" | "running";
  /** Stage of an in-flight agent exec against this terminal, else null. */
  exec_stage?: "queued" | "executing" | null;
  /**
   * Which surface the session's process runs behind: "chat" (structured
   * stream-json driver) or "term" (a PTY). Server truth — the pane renders
   * whichever the daemon says. Optional on old daemons (= "term").
   */
  ui?: "chat" | "term";
  /** Whether this agent can run as a chat session (drives the toggle). */
  chat_capable?: boolean;
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

/** The agent CLI behind a session ("claude" until the server says otherwise). */
export function agentKind(s: Session): string {
  return s.kind === "agent" ? (s.agent_kind ?? "claude") : "shell";
}

/** True for agents with no hook integration yet (state is honestly unknown).
 *  Chat sessions always have protocol-derived state, whatever the agent. */
function unintegrated(s: Session): boolean {
  return s.kind === "agent" && agentKind(s) !== "claude" && s.ui !== "chat";
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
      if (!s.alive) return "";
      // Hook-less agents (codex, gemini) never learn their state: a muted
      // filled dot — honestly unknown, not perpetually "starting". Claude's
      // pre-hook moment stays the hollow provisional ring.
      return unintegrated(s) ? "unk" : "starting";
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
      if (!s.alive) return "exited";
      return unintegrated(s) ? `state unknown (no ${agentKind(s)} integration yet)` : "starting…";
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
  /** The daemon capped a very large directory; some entries are omitted. */
  truncated?: boolean;
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

/** Launcher extras for kind-"agent" spawns (POST /sessions passthrough). */
export interface AgentSpawn {
  /** Which agent CLI ("claude" when omitted). */
  agent?: string;
  /** Claude session id to resume (`claude --resume <id>`). */
  resume?: string;
}

export async function createSession(
  workspaceId: string,
  kind: SessionKind = "shell",
  name: string | null = null,
  size: { cols: number; rows: number } | null = null,
  spawn: AgentSpawn = {},
  /**
   * Whether this agent can run as a structured chat session (AgentInfo.
   * chatCapable, version-gated by the daemon). `false` routes a would-be chat
   * spawn straight to the terminal instead of eating the 20s handshake
   * watchdog before degrading; `undefined` (catalog not loaded) trusts the
   * default view, as before.
   */
  chatCapable?: boolean,
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
  const extras: Record<string, string> = {};
  if (kind === "agent") {
    if (spawn.agent !== undefined && spawn.agent !== "claude") extras.agent = spawn.agent;
    if (spawn.resume !== undefined) extras.resume = spawn.resume;
    // New agent sessions open in the structured chat view by default (the
    // agents.defaultView setting); the terminal is one pane-bar toggle away,
    // and the daemon degrades to a PTY on its own if the protocol handshake
    // fails. An agent the catalog knows is NOT chat-capable (outdated CLI)
    // skips chat entirely — it would only handshake-watchdog then degrade.
    if (
      ["claude", "codex"].includes(spawn.agent ?? "claude") &&
      getSetting("agents.defaultView") === "chat" &&
      chatCapable !== false
    ) {
      extras.ui = "chat";
    }
  }
  // Every spawn (shell AND agent) carries the UI's current scheme: the
  // daemon's shims inject it so TUIs boot themed to match. The SETTINGS
  // store resolves it (appearance.theme system|light|dark → mode), so an
  // explicit user choice beats the OS scheme; read at call time — either
  // may have flipped since module load.
  extras.theme = resolvedTheme();
  return json(
    await api("/sessions", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ workspace_id: workspaceId, kind, name, ...dims, ...extras }),
    }),
  );
}

export async function deleteSession(id: string): Promise<void> {
  await json<void>(await api(`/sessions/${id}`, { method: "DELETE" }));
}

/**
 * A 409 from POST /sessions/{id}/view. `busy` splits the two conflicts apart:
 * `true` = the agent is mid-task (the caller confirms and retries with force);
 * `false` = a switch for this session is already in flight (a duplicate the
 * caller drops quietly — no toast, no confirm).
 */
export class ViewSwitchConflict extends ApiError {
  readonly busy: boolean;
  constructor(message: string, busy: boolean) {
    super(409, message);
    this.name = "ViewSwitchConflict";
    this.busy = busy;
  }
}

/** Session ids with a chat⇄terminal view-switch POST in flight. The pane bar's
 *  toggle disables itself for these (App's switchView owns the set) so a second
 *  click can't fire a concurrent switch the server would only 409. */
export const switchingViews = writable<ReadonlySet<string>>(new Set());

/**
 * Switch a session between the chat and terminal surfaces. The daemon stops
 * the current process and resumes the same conversation in the other mode
 * (same session id). A 409 is raised as a ViewSwitchConflict whose `busy` tells
 * mid-task (confirm + force) apart from an already-in-flight switch (drop it).
 */
export async function switchSessionView(
  id: string,
  ui: "chat" | "term",
  force = false,
): Promise<void> {
  const res = await api(`/sessions/${id}/view`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    // Carry the UI's scheme so the server respawns the process themed to match
    // (same source createSession uses); harmless on servers that ignore it.
    body: JSON.stringify({ ui, force, theme: resolvedTheme() }),
  });
  if (res.status === 409) {
    let busy = false;
    let message = "view switch conflict";
    try {
      const body = (await res.json()) as { error?: string; busy?: boolean };
      if (typeof body.error === "string") message = body.error;
      busy = body.busy === true;
    } catch {
      // non-JSON body; keep the defaults
    }
    throw new ViewSwitchConflict(message, busy);
  }
  await json<void>(res);
}

/** Fork the conversation at a checkpoint (claude chat sessions): the files
 *  were already restored through the chat socket; this respawns the driver
 *  with the transcript truncated at `resumeAt`. */
export async function rewindSession(id: string, resumeAt: string): Promise<void> {
  await json<void>(
    await api(`/sessions/${id}/rewind`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ resume_at: resumeAt }),
    }),
  );
}

/** Pin a user-chosen display name on a session (any kind — the app owns
 *  renaming; only claude has an in-TUI /rename, and it shouldn't be the
 *  only way). The pin outranks every derived name on every surface. */
export async function renameSession(id: string, name: string): Promise<void> {
  await json<void>(
    await api(`/sessions/${id}`, {
      method: "PATCH",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name }),
    }),
  );
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
