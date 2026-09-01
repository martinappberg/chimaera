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
const DETACHED_KEY = "chimaera.dt";

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
  const params = new URLSearchParams(location.hash.slice(1));
  const tokenFromHash = params.get("token");
  const wsFromHash = params.get("ws");
  const hostFromHash = params.get("host");
  const winFromHash = params.get("win");
  const homeHubFromHash = params.get("hub") === "1";
  const detachedFromHash = params.get("dt") === "1";
  const jobFromHash = params.get("job");
  const nodeFromHash = params.get("node");
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
  if (
    tokenFromHash !== null ||
    wsFromHash !== null ||
    hostFromHash !== null ||
    winFromHash !== null ||
    homeHubFromHash ||
    jobFromHash !== null ||
    nodeFromHash !== null
  ) {
    history.replaceState(null, "", location.pathname + location.search);
  }
  return tokenFromHash ?? sessionStorage.getItem(TOKEN_KEY);
}

let token = initFromHash();

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
  return sessionStorage.getItem(HOST_KEY) ?? "local";
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
 *  `job=`/`node=` hash params). Orientation only — the daemon's own
 *  `/compute` `self` block is the authoritative "am I inside a job" fact
 *  (windows opened from within a compute window may not carry the params). */
export interface JobContext {
  jobId: string;
  node: string | null;
}

/** Non-null when this window was opened job-scoped (a compute-node session). */
export function getJobContext(): JobContext | null {
  const jobId = sessionStorage.getItem(JOB_KEY);
  if (jobId === null) return null;
  return { jobId, node: sessionStorage.getItem(NODE_KEY) };
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

export class ApiError extends Error {
  readonly status: number;

  constructor(status: number, message: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
  }
}

/** Fetch wrapper for /api/v1 that attaches the Bearer token. */
export async function api(path: string, init: RequestInit = {}): Promise<Response> {
  const headers = new Headers(init.headers);
  if (token) {
    headers.set("Authorization", `Bearer ${token}`);
  }
  const res = await fetch(`/api/v1${path}`, { ...init, headers });
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
