/**
 * The agent launcher's client side (docs/design/README.md "The agent launcher"):
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
  /** The login shell couldn't answer whether it's installed (plain words,
   *  e.g. it took too long to start). Not "not installed": the way on is a
   *  retry of the check (`recheckAgent`), never an install. */
  unchecked: string | null;
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
  setup?: { id: string; running: boolean; phase: string };
  installation?: { sessionId: string; running: boolean; exitStatus: number | null };
  /** Installed and strictly older than `latestVersion` — the update
   *  affordance: one-click for a managed binary (POST update), quiet
   *  information for the user's own. Never guessed: unparseable versions
   *  stay false. */
  updateAvailable: boolean;
  /** Whether the CLI reports being signed in on the host — its own status
   *  command's verdict (null = unknown, or not installed). */
  signedIn: boolean | null;
  /** How this agent signs in from the app (`agent_auth`): headless under the
   *  daemon with a page (and maybe a code) to open, or in its own terminal
   *  UI; and the sign-in in progress, if any. */
  signIn?: AgentSignIn;
}

export interface AgentSignIn {
  /** Headless under the daemon; the app opens the page. */
  how: "browser";
  command: string;
  acceptsCode: boolean;
  /** Signing in while signed in means a switch of account (false for a CLI
   *  whose headless login only finds the existing one). */
  relogin: boolean;
  operation?: { id: string; running: boolean; phase: string };
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

// --- the launcher's shape: one rail row, the menu behind its chevron --------

/** The id the rail's one row starts: the saved default, or — once the loaded
 *  catalog no longer lists it (a retired agent) — the first installed agent
 *  in catalog order, else the catalog's first, rather than pointing at
 *  nothing. Before the catalog loads the default is taken at its word. */
export function primaryAgentId(defaultId: string, catalog: readonly AgentInfo[] | null): string {
  if (catalog === null || catalog.length === 0 || catalog.some((a) => a.id === defaultId)) return defaultId;
  return (catalog.find((a) => a.installed) ?? catalog[0]).id;
}

/** The launcher menu's two groups, with no built-in bias: the agents this
 *  host has installed (the saved default first, then catalog order), and —
 *  folded behind one row — the ones it has not. One the login shell couldn't
 *  check stays up front: it may well be installed. With nothing installed
 *  there is nothing to prefer, so every agent shows up front. */
export function groupAgents(
  agents: readonly AgentInfo[],
  defaultId: string,
): { installed: AgentInfo[]; others: AgentInfo[] } {
  const lead = (list: readonly AgentInfo[]) => [
    ...list.filter((a) => a.id === defaultId),
    ...list.filter((a) => a.id !== defaultId),
  ];
  const front = (a: AgentInfo) => a.installed || a.unchecked !== null;
  const installed = agents.filter(front);
  if (installed.length === 0) return { installed: lead(agents), others: [] };
  return { installed: lead(installed), others: agents.filter((a) => !front(a)) };
}

/** What the rail's row does when clicked: start the agent, or — when it
 *  could only open a doomed pane — the step that unblocks it (a check the
 *  login shell couldn't finish, a curated install / chat setup in the setup
 *  dialog, else Settings → Agents). An unloaded catalog starts: the daemon
 *  answers honestly. */
export type QuickAction = "start" | "check" | "install" | "setup-chat" | "setup";

export function quickAction(a: AgentInfo | null, defaultView: "chat" | "terminal"): QuickAction {
  if (a === null) return "start";
  if (!a.installed && a.unchecked !== null) return "check";
  if (!a.installed) return a.managedInstall ? "install" : "setup";
  if (a.chatSetupRequired && defaultView === "chat") return a.managedInstall ? "setup-chat" : "setup";
  return "start";
}

/** The rail's one new-agent row: what the main click and the new-agent
 *  shortcut start (its chevron opens the menu for everything else). */
export interface PrimaryStart {
  id: string;
  /** Catalog name, else the built-in product name. */
  name: string;
  action: QuickAction;
  info: AgentInfo | null;
}

export function primaryStart(
  catalog: readonly AgentInfo[] | null,
  defaultId: string,
  defaultView: "chat" | "terminal",
  fallbackName: (id: string) => string = (id) => id,
): PrimaryStart {
  const id = primaryAgentId(defaultId, catalog);
  const info = catalog?.find((a) => a.id === id) ?? null;
  return { id, name: info?.name ?? fallbackName(id), action: quickAction(info, defaultView), info };
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
    const setup = typeof a.setup === "object" && a.setup !== null ? a.setup as Record<string, unknown> : null;
    return [
      {
        installation: operation && typeof operation.session_id === "string" ? {
          sessionId: operation.session_id, running: operation.running === true,
          exitStatus: typeof operation.exit_status === "number" ? operation.exit_status : null,
        } : undefined,
        setup: setup && typeof setup.id === "string" && setup.id.length <= 128 && typeof setup.phase === "string"
          ? { id: setup.id, phase: setup.phase, running: setup.running === true } : undefined,
        id: a.id,
        name: typeof a.name === "string" ? a.name : a.id,
        installed: a.installed === true,
        unchecked: a.installed !== true && typeof a.unchecked === "string" ? a.unchecked.slice(0, 512) : null,
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
        signedIn: typeof a.signed_in === "boolean" ? a.signed_in : null,
        signIn: normalizeSignIn(a.sign_in),
      },
    ];
  });
}

function normalizeSignIn(raw: unknown): AgentSignIn | undefined {
  if (typeof raw !== "object" || raw === null) return undefined;
  const s = raw as Record<string, unknown>;
  if (s.how !== "browser") return undefined;
  const op = typeof s.operation === "object" && s.operation !== null ? s.operation as Record<string, unknown> : null;
  return {
    how: s.how,
    command: typeof s.command === "string" ? s.command.slice(0, 128) : "",
    acceptsCode: s.accepts_code === true,
    relogin: s.relogin !== false,
    operation: op && typeof op.id === "string" && op.id.length <= 128 && typeof op.phase === "string"
      ? { id: op.id, phase: op.phase, running: op.running === true } : undefined,
  };
}

/** What a re-check found (`recheckAgent`). */
export interface AgentCheck {
  installed: boolean;
  path: string | null;
  version: string | null;
  unchecked: string | null;
}

/** Each agent's Retry in flight, shared by every surface that offers one
 *  (the rail, the menu, Settings, the setup dialog): one retry reads
 *  "checking…" everywhere, and a second click joins it. */
const rechecks = new Map<string, Promise<AgentCheck>>();
const recheckIds = writable<ReadonlySet<string>>(new Set());
export const agentRechecks = { subscribe: recheckIds.subscribe };

/** POST /api/v1/agents/{id}/detect — look for one agent again, now, giving
 *  its login shell the patient budget (the Retry of "couldn't check"), then
 *  fold the answer into the shared catalog so every surface agrees. */
export function recheckAgent(agentId: string): Promise<AgentCheck> {
  const joined = rechecks.get(agentId);
  if (joined !== undefined) return joined;
  const run = detectAgain(agentId).finally(() => {
    rechecks.delete(agentId);
    recheckIds.set(new Set(rechecks.keys()));
  });
  rechecks.set(agentId, run);
  recheckIds.set(new Set(rechecks.keys()));
  return run;
}

/** Allows the daemon's whole patient budget plus the trip. */
async function detectAgain(agentId: string): Promise<AgentCheck> {
  const body = await json<Record<string, unknown>>(
    await api(`/agents/${encodeURIComponent(agentId)}/detect`, {
      method: "POST",
      signal: AbortSignal.timeout(45_000),
    }),
  );
  const check: AgentCheck = {
    installed: body.installed === true,
    path: typeof body.path === "string" ? body.path : null,
    version: typeof body.version === "string" ? body.version : null,
    unchecked: body.installed !== true && typeof body.unchecked === "string" ? body.unchecked.slice(0, 512) : null,
  };
  // The daemon cached that answer: a plain read now returns it everywhere.
  await pollAgents(AbortSignal.timeout(15_000)).catch(() => {});
  return check;
}

/** Fold one agent's fresh sign-in verdict into the shared catalog, so every
 *  surface reading it (launcher rows, Settings, a chat's card) agrees without
 *  a full re-detect. */
export function patchAgentSignedIn(id: string, signedIn: boolean | null): void {
  catalog.update((rows) => rows.map((a) => (a.id === id ? { ...a, signedIn } : a)));
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
  /** The Chimaera-managed worktree the conversation ran in, only while it
   *  still exists (reopening resumes there server-side); else null. */
  worktree: string | null;
}

/** GET /api/v1/recents — the workspace's ended agent conversations. */
export async function listRecents(workspaceId: string): Promise<RecentConvo[]> {
  const q = new URLSearchParams({ workspace_id: workspaceId });
  return parseRecents(await json<unknown>(await api(`/recents?${q.toString()}`)));
}

/** Defensive parse of a /recents body (old daemons omit newer fields). */
export function parseRecents(body: unknown): RecentConvo[] {
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
        worktree: typeof r.worktree === "string" && r.worktree !== "" ? r.worktree : null,
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

/** The bare version number, wherever the CLI buried it ("codex-cli 0.52.0",
 *  "2.1.202 (Claude Code)"). Shared by the launcher and both settings panels. */
export function versionNumber(version: string): string {
  return releaseCore(versionToken(version));
}

/** The digit-leading token of a `--version` line, build hash included. */
export function versionToken(version: string): string {
  return version.split(" ").find((t) => /^\d/.test(t)) ?? version.split(" ")[0];
}

/** A build with a hash (Cursor's `2026.10.14-941492e`) reads as its X.Y.Z
 * core; the full token stays in the tooltip. Same rule as the daemon's
 * `agent_updates::release_core`: a git hash may be all digits, so a 7–40
 * character hex suffix in either case counts; `-beta` and shorter suffixes
 * stay whole. */
export function releaseCore(token: string): string {
  const m = /^(\d+\.\d+\.\d+)-[0-9A-Fa-f]{7,40}$/.exec(token);
  return m ? m[1] : token;
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
