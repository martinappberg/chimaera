/**
 * Client half of the browser-pane reverse proxy: mint/refresh proxy sessions
 * on the daemon, poll health, and carry per-tab page titles for the tab bar.
 *
 * A BrowserTab persists only its TARGET (host:port) — the proxy id is a
 * transport detail minted here on demand, so daemon restarts and idle expiry
 * heal invisibly (the view just re-mints and reloads).
 */

import { writable } from "svelte/store";
import { api } from "../net/api";

export interface ProxySession {
  id: string;
  /** Iframe base: `/proxy/{id}` (append the app-internal path). */
  base: string;
}

/** Thrown when the daemon wants an explicit user confirmation for a target
 *  outside the auto allowlist (not loopback / this host / a compute node). */
export class ConfirmRequired extends Error {
  constructor(detail: string) {
    super(detail);
    this.name = "ConfirmRequired";
  }
}

/**
 * Mint (or refresh — the daemon is idempotent per target) a proxy session.
 * `confirm` asserts the user approved a non-allowlisted target.
 */
/** Sessions this window minted, by "host:port" — the sweep's bookkeeping. */
const minted = new Map<string, string>();

export async function mintProxy(
  host: string,
  port: number,
  confirm = false,
): Promise<ProxySession> {
  const res = await api("/proxy", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ host, port, confirm }),
  });
  const body: unknown = await res.json().catch(() => ({}));
  const record = (typeof body === "object" && body !== null ? body : {}) as Record<
    string,
    unknown
  >;
  if (res.status === 403 && record.error === "confirm_required") {
    throw new ConfirmRequired(typeof record.detail === "string" ? record.detail : "");
  }
  if (!res.ok || typeof record.id !== "string") {
    throw new Error(
      typeof record.error === "string" ? record.error : `proxy mint failed (${res.status})`,
    );
  }
  minted.set(`${host}:${port}`, record.id);
  return { id: record.id, base: `/proxy/${record.id}` };
}

/**
 * Revoke sessions this window minted whose target no longer appears in any
 * browser tab (`active` holds "host:port" keys). Hygiene, not correctness —
 * the daemon's idle TTL is the backstop, and a session another window still
 * uses transparently re-mints there.
 */
export function sweepProxies(active: ReadonlySet<string>): void {
  for (const [target, id] of minted) {
    if (!active.has(target)) {
      minted.delete(target);
      revokeProxy(id);
    }
  }
}

export interface ProxyHealth {
  ok: boolean;
  /** "direct" | "relay" when ok. */
  via?: string;
  /** "expired" | "unreachable" when not. */
  error?: string;
  detail?: string;
}

/** Dial the target through the daemon and report — doubles as the mounted
 *  pane's keep-alive (refreshes the proxy session's idle clock). */
export async function proxyHealth(id: string): Promise<ProxyHealth> {
  const res = await api(`/proxy/${encodeURIComponent(id)}/health`);
  if (res.status === 404) return { ok: false, error: "expired" };
  const body: unknown = await res.json().catch(() => null);
  if (typeof body === "object" && body !== null && "ok" in body) {
    return body as ProxyHealth;
  }
  return { ok: false, error: "unreachable", detail: `health check failed (${res.status})` };
}

/** Revoke a proxy session (last pane onto the target closed). Best-effort —
 *  idle expiry is the backstop. */
export function revokeProxy(id: string): void {
  void api(`/proxy/${encodeURIComponent(id)}`, { method: "DELETE" }).catch(() => {
    // unreachable daemon: the session dies with it anyway
  });
}

/**
 * Live page titles per browser-tab instance id, read by the tab bar (the
 * browser equivalent of session display names). Views set/clear their own
 * entry; a missing entry falls back to host:port.
 */
export const browserTitles = writable<ReadonlyMap<string, string>>(new Map());

export function setBrowserTitle(tabId: string, title: string | null): void {
  browserTitles.update((m) => {
    const cur = m.get(tabId);
    if (title === null ? cur === undefined : cur === title) return m;
    const next = new Map(m);
    if (title === null) next.delete(tabId);
    else next.set(tabId, title);
    return next;
  });
}

/** A target formatted for people: `localhost:8888`, `sh03-09n14:8080`. */
export function targetLabel(host: string, port: number): string {
  return host === "" ? "Browser" : `${host}:${port}`;
}

/** Loopback spellings a printed URL uses for "this machine". */
export function isLoopbackHost(host: string): boolean {
  const h = host.toLowerCase();
  return h === "localhost" || h === "::1" || /^127(\.\d{1,3}){3}$/.test(h);
}

/**
 * Which of `hosts` answer on `port`, probed through the daemon (mint +
 * health — compute nodes are auto-allowlisted, and a hit's proven route,
 * relay included, stays cached on the session the pane will then use).
 * Misses are revoked immediately.
 */
export async function probeNodes(hosts: string[], port: number): Promise<string[]> {
  const hits = await Promise.all(
    hosts.map(async (host) => {
      try {
        const session = await mintProxy(host, port);
        const health = await proxyHealth(session.id);
        if (health.ok) return host;
        minted.delete(`${host}:${port}`);
        revokeProxy(session.id);
        return null;
      } catch {
        return null;
      }
    }),
  );
  return hits.filter((h): h is string => h !== null);
}

/**
 * Parse an address-bar entry (or a detected URL) into a proxy target. Accepts
 * `host:port`, `host:port/path`, and full http(s) URLs; a bare `:8888` or
 * `8888` means localhost. Returns null when it isn't an address.
 */
export function parseAddress(
  input: string,
): { host: string; port: number; path: string } | null {
  const raw = input.trim();
  if (raw === "") return null;
  const bare = /^:?(\d{2,5})$/.exec(raw);
  if (bare !== null) {
    const port = Number.parseInt(bare[1], 10);
    return port > 0 && port <= 65535 ? { host: "localhost", port, path: "/" } : null;
  }
  const withScheme = /^https?:\/\//.test(raw) ? raw : `http://${raw}`;
  let url: URL;
  try {
    url = new URL(withScheme);
  } catch {
    return null;
  }
  if (url.hostname === "") return null;
  const port =
    url.port !== "" ? Number.parseInt(url.port, 10) : url.protocol === "https:" ? 443 : 80;
  if (!(port > 0 && port <= 65535)) return null;
  return {
    host: url.hostname,
    port,
    path: `${url.pathname}${url.search}` || "/",
  };
}
