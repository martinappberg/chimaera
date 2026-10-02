import { daemonPath, gatewayPrefix, gatewayWorkspace, isBrowserGateway } from "./base";

import { PlacementError, workspaceHeaders } from "./placement";

import { writable } from "svelte/store";

import { healthPollDelayMs, startVisibilityPoll, type PollHandle } from "./poll";
import { recordLinkRtt } from "./rtt";

const TOKEN_KEY = "chimaera:token";
const WS_KEY = "chimaera.ws";
const HOST_KEY = "chimaera.host";
/** Same key viewState.windowKey() reads — the hash seeds it. */
const WIN_KEY = "chimaera.win";
/** The singleton unused native launcher (local Home or remote detail). */
const HOME_HUB_KEY = "chimaera.homeHub";
/** Set when this window was opened onto a compute-node daemon (Mode 2). */
const JOB_KEY = "chimaera.job";
const NODE_KEY = "chimaera.node";
/** The cluster workspace a job window serves (`cws=`), for stop/continue. */
const CWS_KEY = "chimaera.cws";
const DETACHED_KEY = "chimaera.dt";
/** A terminal-only window's session (`term=`): the page shows just it. */
const TERM_KEY = "chimaera.term";

/**
 * Read the access token, workspace id, host label, and window id from the
 * URL fragment (#token=...&ws=...&host=...&win=...&hub=1) once, persist them to
 * sessionStorage, and strip the fragment from the address bar. Falls back
 * to previously stored values on reload.
 *
 * `win` is the window's stable view-state identity. sessionStorage alone
 * cannot carry it across an app restart (new webview) or a re-home to a
 * moved daemon port (new origin), so the shell — and the re-home paths —
 * put it in the hash; adopting it here is what makes a reopened window THE
 * SAME window, layout and all.
 */
function initFromHash(): string | null {
  if (isBrowserGateway()) {
    const host = gatewayPrefix();
    if (sessionStorage.getItem("chimaera.gatewayHost") !== host) {
      for (const key of [TOKEN_KEY, WS_KEY, HOST_KEY, WIN_KEY, HOME_HUB_KEY, JOB_KEY, NODE_KEY, DETACHED_KEY]) sessionStorage.removeItem(key);
      sessionStorage.setItem("chimaera.gatewayHost", host);
    }
    // Workspace/window fragments remain useful; only credentials are ignored.
  }
  const params = new URLSearchParams(location.hash.slice(1));
  const tokenFromHash = isBrowserGateway() ? null : params.get("token");
  const wsFromHash = gatewayWorkspace() ?? params.get("ws");
  const hostFromHash = params.get("host");
  const winFromHash = params.get("win");
  const homeHubFromHash = params.get("hub") === "1";
  const detachedFromHash = params.get("dt") === "1";
  const jobFromHash = params.get("job");
  const nodeFromHash = params.get("node");
  const cwsFromHash = params.get("cws");
  const termFromHash = params.get("term");
  // A file the shell asks this window to show once (a cluster file peek).
  // Held in memory only, never sessionStorage: a reload must not reopen it.
  const openFromHash = params.get("open");
  if (openFromHash !== null && openFromHash.startsWith("/")) pendingOpen = openFromHash;
  if (detachedFromHash) {
    sessionStorage.setItem(DETACHED_KEY, "1");
    // A detached browser popup is an auxiliary context: it inherited a CLONE
    // of the opener's sessionStorage (noopener would make popup-blocking
    // undetectable, so the opener keeps the handle and severs it instead).
    // The window-scoped keys are overwritten from the hash above, but chat
    // drafts are keyed by SESSION — stale copies here would resurrect text
    // the user already sent or rewrote in the source window. Purge them.
    const stale: string[] = [];
    for (let i = 0; i < sessionStorage.length; i++) {
      const k = sessionStorage.key(i);
      if (k !== null && k.startsWith("chimaera.chatDraft.")) stale.push(k);
    }
    for (const k of stale) sessionStorage.removeItem(k);
  } else if (winFromHash !== null) {
    // A hash that names a window without dt=1 is authoritative: this window
    // is (or became) a plain workbench.
    sessionStorage.removeItem(DETACHED_KEY);
  }
  if (homeHubFromHash) {
    // Each daemon origin has its own sessionStorage, so clear anything an
    // earlier visit to that origin left behind before applying this route.
    sessionStorage.removeItem(WS_KEY);
    sessionStorage.removeItem(JOB_KEY);
    sessionStorage.removeItem(NODE_KEY);
    sessionStorage.removeItem(CWS_KEY);
    if (hostFromHash === null) sessionStorage.removeItem(HOST_KEY);
    if (wsFromHash === null) {
      sessionStorage.setItem(HOME_HUB_KEY, "1");
    } else {
      // A workspace consumes the launcher window. If that workspace later
      // disappears, the native shell can explicitly reclaim this identity.
      sessionStorage.removeItem(HOME_HUB_KEY);
    }
  } else if (winFromHash !== null) {
    // A separately opened native workbench must not inherit hub semantics.
    sessionStorage.removeItem(HOME_HUB_KEY);
  }
  if (tokenFromHash !== null) {
    sessionStorage.setItem(TOKEN_KEY, tokenFromHash);
  }
  if (wsFromHash !== null) {
    sessionStorage.setItem(WS_KEY, wsFromHash);
  }
  if (hostFromHash !== null) {
    sessionStorage.setItem(HOST_KEY, hostFromHash);
  }
  if (winFromHash !== null && /^[A-Za-z0-9_-]{1,64}$/.test(winFromHash)) {
    sessionStorage.setItem(WIN_KEY, winFromHash);
  }
  if (jobFromHash !== null) {
    sessionStorage.setItem(JOB_KEY, jobFromHash);
  }
  if (nodeFromHash !== null) {
    sessionStorage.setItem(NODE_KEY, nodeFromHash);
  }
  if (cwsFromHash !== null && /^[A-Za-z0-9_-]{1,64}$/.test(cwsFromHash)) {
    sessionStorage.setItem(CWS_KEY, cwsFromHash);
  }
  if (termFromHash !== null && /^[A-Za-z0-9_-]{1,64}$/.test(termFromHash)) {
    sessionStorage.setItem(TERM_KEY, termFromHash);
  }
  if (
    params.has("token") ||
    wsFromHash !== null ||
    hostFromHash !== null ||
    winFromHash !== null ||
    homeHubFromHash ||
    jobFromHash !== null ||
    nodeFromHash !== null ||
    cwsFromHash !== null ||
    termFromHash !== null ||
    openFromHash !== null
  ) {
    history.replaceState(null, "", location.pathname + location.search);
  }
  return isBrowserGateway() ? null : tokenFromHash ?? sessionStorage.getItem(TOKEN_KEY);
}

/** Set by `initFromHash` before `token` (declared first so the hoisted
 *  bootstrap can assign it). */
let pendingOpen: string | null = null;
let token = initFromHash();

/**
 * The session a terminal-only window shows (the `term=` hash param — a
 * cluster's login-node terminal), or null for every other window. Kept in
 * sessionStorage so a reload stays a terminal window.
 */
export function terminalWindowSession(): string | null {
  return sessionStorage.getItem(TERM_KEY);
}

/**
 * The absolute path the shell asked this window to open (the `open=` hash
 * param — a cluster file peek copied to this machine), once: the first call
 * returns it, later calls null. The caller checks it lies inside the
 * workspace and exists before opening it.
 */
export function takeOpenRequest(): string | null {
  const p = pendingOpen;
  pendingOpen = null;
  return p;
}

/** The bearer token for this session, if one was provided. */
export function getToken(): string | null {
  return token;
}

/** True in the unused native launcher, including while it browses a remote
 * host detail page. Entering a workspace clears this identity. */
/** Whether this window was opened as a DETACHED solo window (the `dt=1`
 *  hash param, carried by the shell's window URL and the browser popup).
 *  Known before the layout blob loads — it is what keeps a solo window off
 *  the workspace-mirror restore fallback. */
export function isDetachedWindow(): boolean {
  return sessionStorage.getItem(DETACHED_KEY) === "1";
}

/** A solo window converted to (or from) a full workbench in place. */
export function setDetachedWindow(detached: boolean): void {
  if (detached) sessionStorage.setItem(DETACHED_KEY, "1");
  else sessionStorage.removeItem(DETACHED_KEY);
}

export function isHomeHub(): boolean {
  return sessionStorage.getItem(HOME_HUB_KEY) === "1";
}

/** A launcher that entered a workspace is now an ordinary workbench window. */
export function leaveHomeHub(): void {
  sessionStorage.removeItem(HOME_HUB_KEY);
}

/** The native shell confirmed that this local empty window reclaimed Home. */
export function reclaimHomeHub(): void {
  sessionStorage.setItem(HOME_HUB_KEY, "1");
}

/**
 * True once any REST call or events socket saw a 401/unauthorized. Browser
 * windows use the manual re-auth page; native remote windows use their
 * host-scoped SSH reconnect. A successful re-auth reloads the window — except
 * a native tunnel healed in place (same port, same token), which releases the
 * latch via {@link clearUnauthorized} instead.
 */
export const unauthorized = writable(false);

/** Mark this window's auth as dead (401 from REST or a WS auth error). */
export function notifyUnauthorized(): void {
  unauthorized.set(true);
}

/** This window's credentials work again without a reload (a native remote
 *  window's in-place tunnel heal). Every other recovery navigates, which
 *  resets the latch by itself. */
export function clearUnauthorized(): void {
  unauthorized.set(false);
}

/**
 * Re-read the token from the URL fragment (the user may have pasted a fresh
 * `chimaera connect` URL into the address bar without reloading). Returns
 * true when a new token was picked up.
 */
export function refreshTokenFromHash(): boolean {
  if (isBrowserGateway()) return false;
  const params = new URLSearchParams(location.hash.slice(1));
  const fresh = params.get("token");
  if (fresh === null || fresh === token) return false;
  token = fresh;
  sessionStorage.setItem(TOKEN_KEY, fresh);
  history.replaceState(null, "", location.pathname + location.search);
  return true;
}

/**
 * What the user calls the machine this window is connected to: the ssh alias
 * passed by `chimaera connect` (e.g. "cluster"), or "local" for a daemon
 * reached without a tunnel. The raw hostname stays available as hover detail.
 */
export function getHostLabel(): string {
  return sessionStorage.getItem(HOST_KEY) ?? (isBrowserGateway() ? gatewayHostLabel() : "local");
}

/** The explicit host of a browser view (`/app/{host}/`); undefined for a
 *  project view, which follows its project. */
function gatewayHost(): string | undefined {
  return /^\/app\/([A-Za-z0-9_-]{1,128})(?:\/|$)/.exec(location.pathname)?.[1];
}

/** A browser view's machine in plain words: a project view follows the
 *  project wherever it runs; an explicit host names the computer, the cloud
 *  or the cluster alias. Never "local" (that key means this very machine). */
function gatewayHostLabel(): string {
  const host = gatewayHost();
  if (host === undefined) return "This project";
  if (host.startsWith("device-")) return "Your computer";
  if (host.startsWith("worker-")) return "The cloud";
  return host === "local" ? "Your computer" : host;
}

/**
 * True when this window is connected to a REMOTE daemon (over an ssh tunnel),
 * false for a local daemon. Both remote producers set `host=` in the URL hash
 * — the native shell for tunnelled windows and `chimaera connect` for the
 * browser — so its absence means local. Gates remote-only affordances like the
 * Download menu entries (downloading a file to the machine it already lives on
 * is pointless).
 */
export function isRemoteHost(): boolean {
  return getHostLabel() !== "local";
}

/** The Slurm job a job-scoped window was opened onto (from the shell's
 *  `job=`/`node=`/`cws=` hash params). Orientation only — the daemon's own
 *  `/compute` `self` block is the authoritative "am I inside a job" fact
 *  (windows opened from within a compute window may not carry the params). */
export interface JobContext {
  jobId: string;
  node: string | null;
  /** The cluster workspace this job serves (`w-…`) — what stop, continue
   *  and reconnect name to the shell. Null on a window from an older build. */
  cws: string | null;
}

/** Non-null when this window was opened job-scoped (a workspace's job). */
export function getJobContext(): JobContext | null {
  const jobId = sessionStorage.getItem(JOB_KEY);
  if (jobId === null) return null;
  return {
    jobId,
    node: sessionStorage.getItem(NODE_KEY),
    cws: sessionStorage.getItem(CWS_KEY),
  };
}

/** The workspace id this tab is scoped to, if any (window = workspace). */
export function getActiveWorkspaceId(): string | null {
  return sessionStorage.getItem(WS_KEY);
}

/** Persist the tab's active workspace id; null clears it. */
export function setActiveWorkspaceId(id: string | null): void {
  if (id === null) {
    sessionStorage.removeItem(WS_KEY);
  } else {
    sessionStorage.setItem(WS_KEY, id);
  }
}

/** A refusal identifies only that this daemon does not own the project.
 * The daemon's host class cannot identify the other owner: it may be the
 * cloud or another computer. Callers with verified placement pass it explicitly.
 */
export function ownerElsewhere(): null {
  return null;
}

/** "This project is running … right now", naming the machine when known.
 *  "another computer" is a second computer that holds the project, never a
 *  device merely viewing it. */
export function runningElsewhere(where: "cloud" | "computer" | "other" | null): string {
  switch (where) {
    case "cloud":
      return "This project is running in the cloud right now.";
    case "computer":
      return "This project is running on your computer right now.";
    case "other":
      return "This project is running on another computer right now.";
    default:
      return "This project is running somewhere else right now.";
  }
}

/** Plain words for the daemon's project-connection codes, which are wire
 *  identifiers and must never reach the screen as-is. */
const PLAIN_ERRORS: Record<string, string> = {
  project_unavailable: "This project isn’t reachable right now.",
  remote_unavailable: "Your project is reconnecting.",
  workspace_scope_changed: "Your project is reconnecting.",
  workspace_unavailable: "Your project is reconnecting.",
  worker_asleep: "The cloud machine is asleep.",
  outside_project: "This file is outside the project.",
};
/** Codes meaning "this daemon may not run that project": the sentence names
 *  where it runs instead ({@link runningElsewhere}). */
const ELSEWHERE_CODES = new Set(["workspace_owned_elsewhere", "read_only"]);
/** The project's owner has a different file at that path, which this window
 *  may not show: the sentence names where the owner is. */
const OTHER_FILE = "on_other_machine";

function otherFile(where: "cloud" | "computer" | "other" | null): string {
  switch (where) {
    case "cloud":
      return "This file is in the cloud and can’t be opened here.";
    case "computer":
      return "This file is on your computer and can’t be opened here.";
    default:
      return "This file is on the other machine and can’t be opened here.";
  }
}

/** A daemon error message in plain words (known codes mapped, else as-is).
 *  `where` names the project's owner when the caller knows it. */
export function plainError(message: string, where: "cloud" | "computer" | "other" | null = ownerElsewhere()): string {
  if (ELSEWHERE_CODES.has(message)) return runningElsewhere(where);
  if (message === OTHER_FILE) return otherFile(where);
  return PLAIN_ERRORS[message] ?? message;
}

/** Codes that say where the project is right now rather than that something
 *  went wrong: its owner sleeps, is reconnecting, or is not reachable while
 *  the project is routed elsewhere. A surface that read something and got one
 *  of these shows a quiet note and reads again once the owner answers; it
 *  never paints it as an error. */
const STATE_CODES = new Set(["worker_asleep", "project_unavailable", "remote_unavailable", "workspace_scope_changed", "workspace_unavailable"]);

/** The line for a sleeping owner: the chat's footer says the same, and a
 *  dashboard or panel has the room for the way out. */
export const ASLEEP_NOTE = "Asleep in the cloud. Send a message to wake it.";

/** The failure is a sleeping cloud machine: its wake is announced (a socket
 *  ready, a placement read), so nothing needs to poll for it. */
export function isOwnerAsleep(e: unknown): boolean {
  return e instanceof ApiError && e.code === "worker_asleep";
}

/** The quiet note a state-code failure reads as ("Asleep in the cloud. …",
 *  "This project isn’t reachable right now."), or null for anything else —
 *  a real failure, which keeps its error styling. */
export function projectStateNote(e: unknown): string | null {
  if (!(e instanceof ApiError) || e.code === null || !STATE_CODES.has(e.code)) return null;
  return e.code === "worker_asleep" ? ASLEEP_NOTE : e.message;
}

export class ApiError extends Error {
  readonly status: number;
  /** The daemon's code when it sent one this module knows (additive). */
  readonly code: string | null;

  constructor(status: number, message: string) {
    super(plainError(message));
    this.name = "ApiError";
    this.status = status;
    this.code = message in PLAIN_ERRORS || ELSEWHERE_CODES.has(message) || message === OTHER_FILE ? message : null;
  }
}

/** Fetch wrapper for /api/v1 that attaches the Bearer token. */
export async function api(path: string, init: RequestInit = {}): Promise<Response> {
  const headers = new Headers(init.headers);
  if (token) {
    headers.set("Authorization", `Bearer ${token}`);
  }
  if (isBrowserGateway()) headers.set("X-Chimaera-Browser", "1");
  if (!isBrowserGateway()) {
    const workspace=getActiveWorkspaceId();
    if (workspace !== null && /^[A-Za-z0-9_-]{1,128}$/.test(workspace)) headers.set("X-Chimaera-Viewer-Workspace",workspace);
  }
  try { await workspaceHeaders(headers); }
  catch (error) { if (error instanceof PlacementError && error.status === 401) notifyUnauthorized(); throw error; }
  const res = await fetch(daemonPath(`/api/v1${path}`), { ...init, headers });
  if (res.status === 401) notifyUnauthorized();
  return res;
}

export interface Health {
  name: string;
  version: string;
  /** Daemon build id (undefined from daemons predating build ids). */
  build?: string;
  hostname: string;
  pid: number;
  uptime_secs: number;
}

export async function health(): Promise<Health> {
  // 4s abort: pollHealth arms its next tick only after this settles, so an
  // unbounded hang (a dead tunnel's ~75s TCP connect) would otherwise
  // stretch the 5s recovery cadence to ~80s.
  const started = performance.now();
  const res = await api("/health", { signal: AbortSignal.timeout(4000) });
  if (!res.ok) {
    throw new ApiError(res.status, `health check failed with status ${res.status}`);
  }
  const body = (await res.json()) as Health;
  // Only successful fetches sample the link RTT (net/rtt.ts) — an error
  // status's timing measures the failure, not the link.
  recordLinkRtt(performance.now() - started);
  return body;
}

/**
 * Poll /api/v1/health. Fires immediately, then at `delayMs(hidden)` —
 * re-evaluated per arm, with a catch-up fetch on visibility return (see
 * net/poll.ts). The caller picks the cadence: /ws/events is the real
 * liveness signal, so App passes `healthPollDelayMs` keyed on it. Returns
 * the poll handle — `kick()` requests a prompt (damped) probe on an events
 * transition; `stop()` tears down.
 */
export function pollHealth(
  onResult: (h: Health) => void,
  onError: (e: unknown) => void,
  delayMs: (hidden: boolean) => number = (hidden) => healthPollDelayMs(false, hidden),
): PollHandle {
  let stopped = false;
  const handle = startVisibilityPoll(async () => {
    try {
      const h = await health();
      if (!stopped) onResult(h);
    } catch (e) {
      if (!stopped) onError(e);
    }
  }, delayMs);
  return {
    stop(): void {
      stopped = true;
      handle.stop();
    },
    kick(): void {
      handle.kick();
    },
  };
}
