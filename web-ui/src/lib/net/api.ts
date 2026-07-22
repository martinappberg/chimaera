import { writable } from "svelte/store";

const TOKEN_KEY = "chimaera:token";
const WS_KEY = "chimaera.ws";
const HOST_KEY = "chimaera.host";
/** Same key viewState.windowKey() reads — the hash seeds it. */
const WIN_KEY = "chimaera.win";
/** Set when this window was opened onto a compute-node daemon (Mode 2). */
const JOB_KEY = "chimaera.job";
const NODE_KEY = "chimaera.node";

/**
 * Read the access token, workspace id, host label, and window id from the
 * URL fragment (#token=...&ws=...&host=...&win=...) once, persist them to
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
  const jobFromHash = params.get("job");
  const nodeFromHash = params.get("node");
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

/**
 * True once any REST call or events socket saw a 401/unauthorized. Browser
 * windows use the manual re-auth page; native remote windows use their
 * host-scoped SSH reconnect. Both successful paths reload the window.
 */
export const unauthorized = writable(false);

/** Mark this window's auth as dead (401 from REST or a WS auth error). */
export function notifyUnauthorized(): void {
  unauthorized.set(true);
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
  const res = await api("/health");
  if (!res.ok) {
    throw new ApiError(res.status, `health check failed with status ${res.status}`);
  }
  return (await res.json()) as Health;
}

/**
 * Poll /api/v1/health on an interval. Fires immediately, then every
 * `intervalMs`. Returns a stop function.
 */
export function pollHealth(
  onResult: (h: Health) => void,
  onError: (e: unknown) => void,
  intervalMs = 5000,
): () => void {
  let stopped = false;
  const tick = async () => {
    try {
      const h = await health();
      if (!stopped) onResult(h);
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
