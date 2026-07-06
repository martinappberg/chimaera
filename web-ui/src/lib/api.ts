const TOKEN_KEY = "chimaera:token";
const WS_KEY = "chimaera.ws";

/**
 * Read the access token and workspace id from the URL fragment
 * (#token=...&ws=...) once, persist them to sessionStorage, and strip the
 * fragment from the address bar. Falls back to previously stored values on
 * reload.
 */
function initFromHash(): string | null {
  const params = new URLSearchParams(location.hash.slice(1));
  const tokenFromHash = params.get("token");
  const wsFromHash = params.get("ws");
  if (tokenFromHash !== null) {
    sessionStorage.setItem(TOKEN_KEY, tokenFromHash);
  }
  if (wsFromHash !== null) {
    sessionStorage.setItem(WS_KEY, wsFromHash);
  }
  if (tokenFromHash !== null || wsFromHash !== null) {
    history.replaceState(null, "", location.pathname + location.search);
  }
  return tokenFromHash ?? sessionStorage.getItem(TOKEN_KEY);
}

const token = initFromHash();

/** The bearer token for this session, if one was provided. */
export function getToken(): string | null {
  return token;
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
  return fetch(`/api/v1${path}`, { ...init, headers });
}

export interface Health {
  name: string;
  version: string;
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
