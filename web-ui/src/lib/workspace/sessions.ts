import { writable } from "svelte/store";

import { api, ApiError } from "../net/api";
import { getSetting, resolvedTheme } from "../settings/store.svelte";

export interface Workspace {
  id: string;
  root: string;
  name: string;
  /** Unix seconds of the last open/activity; 0/absent on old daemons. */
  last_opened_at?: number;
  /**
   * The workspace's Mastermind binding: the privileged chat session's id +
   * the ask/auto gating mode its act tools were spawned with. Absent when
   * none is configured (and on old daemons). Exactly one per workspace.
   */
  mastermind?: { session_id: string; mode: "ask" | "auto"; agent?: string };
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
   * Output-recency activity, the busy signal for hook-less agent TUIs
   * (codex/gemini/agy): true while the PTY produced output within the
   * daemon's quiet window (a working TUI streams and animates its spinner
   * continuously), false once it has gone quiet. Null/absent whenever a
   * better signal exists (claude's hooks, chat protocol state) or on old
   * daemons.
   */
  output_active?: boolean | null;
  /**
   * The inverse liveness check for the hooks tier (claude TUIs): true when
   * the record claims "running" but the PTY has been silent past the
   * daemon's stall window (180s) — the state claim is likely stale. Boolean
   * only while the claim is checkable (a live claude TUI in state
   * "running"); null elsewhere, absent on old daemons.
   */
  stalled?: boolean | null;
  /**
   * Live claude-TUI subagents from the SubagentStart/Stop hooks: the
   * agent's own id + label (canonical, never relabeled) and the daemon's
   * epoch-ms start stamp. Null (never []) when none, for chat rows (the
   * chat client derives richer rows from its journal), and for hook-less
   * TUIs; absent on old daemons.
   */
  subagents?: { id: string; label: string; started_at: number }[] | null;
  /**
   * One-line latest-hook summary of what a claude TUI just did ("ran Bash",
   * "editing foo.rs"); replaced per hook, cleared on exit. Null for chat
   * rows and hook-less TUIs; absent on old daemons.
   */
  now_line?: string | null;
  /**
   * The claude-TUI statusline heartbeat: model name, context-window use
   * (whole percent), session cost (dollars, quantized to whole cents). Null
   * for chat rows and hook-less TUIs; absent on old daemons.
   */
  usage?: { model: string | null; context_pct: number | null; cost_usd: number | null } | null;
  /**
   * True when this session is a workspace's Mastermind — the observer, not
   * the observed: the UI keeps flagged rows out of the rail, the dashboard
   * roster/lane, and every recents-like surface (the dashboard dock is its
   * one home). Null on ordinary rows; absent on old daemons.
   */
  mastermind?: boolean | null;
  /**
   * Which surface the session's process runs behind: "chat" (structured
   * stream-json driver) or "term" (a PTY). Server truth — the pane renders
   * whichever the daemon says. Optional on old daemons (= "term").
   */
  ui?: "chat" | "term";
  /**
   * The agent's own post-turn status line (claude post_turn_summary,
   * latest-wins): a one-line "where things stand" shown as the rail row's
   * second line. Chat sessions only; null/absent elsewhere or on old
   * daemons.
   */
  status_detail?: string | null;
  /** Machine-readable category behind status_detail ("review_ready", …). */
  status_category?: string | null;
  /** The latest status flagged "waiting on the user"; the daemon already
   *  folds it into agent_state ("idle_prompt"), carried raw for future
   *  surfaces. Cleared when a new turn starts. */
  status_needs_action?: boolean;
  /**
   * How many background tasks (backgrounded Bash / workflows) this agent has
   * running right now — the daemon's fold of the `background_tasks` level-set.
   * Chat sessions only; null on PTY rows (a TUI's Ctrl-B raises no hook, so
   * null means "unknown", not "none") and absent on old daemons.
   *
   * A count, not the set: it rides every session-list snapshot, and the
   * surfaces reading it only need "is work still going". A view wanting the
   * rows themselves reads its own ChatStore, which has the full set.
   */
  background_running?: number | null;
  /** Whether this agent can run as a chat session (drives the toggle). */
  chat_capable?: boolean;
}

/** The one display name for a session, used identically everywhere. */
export function displayName(s: Session): string {
  return s.display_name ?? s.agent_title ?? s.name;
}

/** True when the session is waiting on the user (drives the aggregate count). */
/**
 * The workspace Mastermind is the observer, not the observed: every roster
 * surface (rail, dashboard roster/lane, home rollups, quick-open) filters
 * flagged rows through THIS predicate — one point of change, so a new
 * surface can't forget the rule and a richer flag never needs a hunt.
 */
export function isMastermind(s: Session): boolean {
  return s.mastermind === true;
}

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
 * True when a session is doing ACTIVE work right now: a shell running a
 * foreground command (OSC 133) or hosting an agent exec, an agent mid-turn, or
 * an agent whose turn ended while backgrounded work keeps running. An idle
 * shell (at the prompt) or a genuinely finished agent is NOT busy — the daemon
 * can restart around it, and the status dot should say so.
 *
 * Background work counts because it does NOT restore across a restart: the
 * tasks are the CLI's children and die with the process. "Mid-turn" alone
 * would call such a session idle and let both callers act on it silently —
 * the × would skip its confirm, and the pre-update warning would undercount.
 */
export function isBusy(s: Session): boolean {
  if (!s.alive) return false;
  if (s.kind !== "agent") {
    return s.phase === "running" || s.exec_stage === "executing";
  }
  // Hook/protocol state is primary wherever it exists; hook-less agent TUIs
  // never reach agent_state "running", so the daemon's output-recency flag
  // is their busy signal — additive, so a future real state for these
  // agents wins the moment it exists (and absent on old daemons).
  return (
    s.agent_state === "running" ||
    s.exec_stage === "executing" ||
    backgrounded(s) ||
    (unintegrated(s) && s.output_active === true)
  );
}

/**
 * Dot modifier class for a session (shared by the rail, pane tabs, and the
 * session strip; see the SessionGlyph state styles).
 */
export function dotState(s: Session): string {
  if (s.kind !== "agent") {
    if (!s.alive) return "";
    // A terminal is "active" (accent) ONLY while a foreground command runs —
    // an OSC 133 "running" phase, or an agent exec against this shell. At the
    // prompt (or with no shell integration) it is idle: a quiet dot, never a
    // perpetual green.
    return s.phase === "running" || s.exec_stage === "executing" ? "alive" : "idle";
  }
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
      // Hook-less agents (codex, gemini) never learn a hook state, but the
      // daemon derives busy/quiet from PTY output recency: working gets the
      // live accent, quiet the calm idle dot (likely at its prompt). Only an
      // old daemon without the signal keeps the muted "honestly unknown"
      // dot. Claude's pre-hook moment stays the hollow provisional ring.
      if (unintegrated(s)) {
        if (s.output_active === true) return "alive";
        if (s.output_active === false) return "idle";
        return "unk";
      }
      return "starting";
  }
}

/**
 * The turn is idle but background work (backgrounded Bash / workflows) is
 * still running — "working off-screen", not finished. Every surface that cues
 * it reads THIS predicate (the rail glyph's muted breathing, the focus-strip
 * chip, the dashboard card's pulsing dot) so they can never disagree.
 *
 * Wire truth, so it holds for a session no window has ever opened — the whole
 * reason the daemon folds the count onto the row instead of leaving it to
 * whichever client happens to be attached.
 */
export function backgrounded(s: Session): boolean {
  return s.alive && s.agent_state !== "running" && (s.background_running ?? 0) > 0;
}

/**
 * Hover tooltip naming the state behind a session dot.
 *
 * Background work is appended rather than folded into the state words: the
 * mark breathes for it, and a pulsing mark whose tooltip reads "finished"
 * looks like a rendering bug. The turn state stays the headline — the two
 * facts are independent ("finished · 2 running in the background").
 */
export function dotTitle(s: Session): string {
  const base = turnDotTitle(s);
  const running = s.background_running ?? 0;
  if (!s.alive || running === 0) return base;
  return `${base} · ${running} running in the background`;
}

/** The turn/phase half of {@link dotTitle}. */
function turnDotTitle(s: Session): string {
  if (s.kind !== "agent") {
    if (!s.alive) return "exited";
    if (s.phase === "running") return "running a command";
    if (s.exec_stage === "executing") return "agent is running a command here";
    return "at the prompt"; // idle: alive but not doing work
  }
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
      if (unintegrated(s)) {
        if (s.output_active === true) return "agent working (terminal activity)";
        if (s.output_active === false) return "quiet — no recent output";
        return `state unknown (no ${agentKind(s)} integration yet)`;
      }
      return "starting…";
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

/**
 * Create `path` (and any missing parents) on the daemon and return its
 * canonical path. The daemon expands a leading "~"; 400 with a message if the
 * directory cannot be created. Idempotent for an existing directory.
 */
export async function fsMkdir(path: string): Promise<string> {
  const body = await json<{ path: string }>(
    await api("/fs/mkdir", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ path }),
    }),
  );
  return body.path;
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

/**
 * Appoint the workspace's Mastermind: the daemon creates the privileged chat
 * session AND binds it in one step, retiring any previous one — a mode change
 * is a re-PUT (a running agent never re-reads its generated settings).
 * Returns the new session row. Errors carry UI-showable messages: 400 (bad
 * mode, agent without a chat driver), 409 (agent binary unavailable), 404
 * (unknown workspace).
 */
export async function putMastermind(
  workspaceId: string,
  body: { agent: string; mode: "ask" | "auto"; model?: string; theme?: "light" | "dark" },
): Promise<Session> {
  return json(
    await api(`/workspaces/${workspaceId}/mastermind`, {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    }),
  );
}

/** Retire the workspace's Mastermind: unbind + end its session (204). */
export async function deleteMastermind(workspaceId: string): Promise<void> {
  await json<void>(await api(`/workspaces/${workspaceId}/mastermind`, { method: "DELETE" }));
}

export async function listSessions(): Promise<Session[]> {
  return json(await api("/sessions"));
}

/** Launcher extras for kind-"agent" spawns (POST /sessions passthrough). */
export interface AgentSpawn {
  /** Which agent CLI ("claude" when omitted). */
  agent?: string;
  /** Native conversation handle (Claude session id / Codex thread id). */
  resume?: string;
  /** A title to seed a resumed conversation with (a recents row's title),
   *  carried across the fresh id an "open recent" mints. Seeds the soft
   *  ai_title server-side — not a hard rename. */
  titleHint?: string;
  /** Explicit surface choice (the launcher's "open" vs its terminal button).
   *  Omitted = the agents.defaultView setting decides. */
  ui?: "chat" | "term";
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
    if (spawn.titleHint !== undefined && spawn.titleHint !== "") extras.title_hint = spawn.titleHint;
    // New agent sessions open in the structured chat view by default (the
    // agents.defaultView setting); an explicit launcher choice (its terminal
    // button vs "open") overrides the setting for that one spawn. The
    // terminal is one pane-bar toggle away either way, and the daemon
    // degrades to a PTY on its own if the protocol handshake fails. An agent
    // the catalog knows is NOT chat-capable (outdated CLI) skips chat
    // entirely — it would only handshake-watchdog then degrade.
    const wantChat =
      spawn.ui !== undefined ? spawn.ui === "chat" : getSetting("agents.defaultView") === "chat";
    if (["claude", "codex"].includes(spawn.agent ?? "claude") && wantChat && chatCapable !== false) {
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
