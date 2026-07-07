/**
 * Bridge to the native shell (Tauri), when this UI runs inside it.
 *
 * The shell exposes `window.__TAURI__` (withGlobalTauri) to daemon-served
 * pages, so the web bundle stays shell-agnostic: every helper here has a
 * browser fallback, and `isNativeShell()` gates the shell-only affordances
 * (remote hosts, real windows). Command and event names are the contract
 * with crates/chimaera-app — change them in lockstep.
 */

import { getToken } from "./api";
import type { Workspace } from "./sessions";

interface TauriGlobal {
  core: { invoke: <T>(cmd: string, args?: Record<string, unknown>) => Promise<T> };
  event: {
    listen: <T>(
      event: string,
      handler: (e: { payload: T }) => void,
    ) => Promise<() => void>;
  };
}

function tauri(): TauriGlobal | null {
  return (window as { __TAURI__?: TauriGlobal }).__TAURI__ ?? null;
}

/** True when running inside the chimaera native shell. */
export function isNativeShell(): boolean {
  return tauri() !== null;
}

/** Connection lifecycle of a saved remote host, as reported by the shell. */
export type HostStatus = "disconnected" | "connecting" | "connected" | "error";

export interface HostState {
  alias: string;
  status: HostStatus;
  /** Local end of the tunnel while connected. */
  local_port: number | null;
  last_connected_at: number | null;
  /** Last connect error, while status is "error". */
  error: string | null;
}

/** Progress of an in-flight connect, mirrored from chimaera-remote phases. */
export interface ConnectProgress {
  alias: string;
  phase: "probing" | "installing" | "starting" | "tunneling";
}

export async function listHosts(): Promise<HostState[]> {
  const t = tauri();
  if (t === null) return [];
  return t.core.invoke<HostState[]>("list_hosts");
}

export async function addHost(alias: string): Promise<HostState> {
  const t = tauri();
  if (t === null) throw new Error("not running in the native shell");
  return t.core.invoke<HostState>("add_host", { alias });
}

export async function removeHost(alias: string): Promise<void> {
  await tauri()?.core.invoke<void>("remove_host", { alias });
}

/**
 * Connect to a saved host (probe, auto-install, start, tunnel). Resolves to
 * the connected state; rejects with the shell's error message on failure.
 * Progress arrives via `onConnectProgress`.
 */
export async function connectHost(alias: string): Promise<HostState> {
  const t = tauri();
  if (t === null) throw new Error("not running in the native shell");
  return t.core.invoke<HostState>("connect_host", { alias });
}

export async function disconnectHost(alias: string): Promise<void> {
  await tauri()?.core.invoke<void>("disconnect_host", { alias });
}

/** The connected host's registered workspaces (proxied through the shell). */
export async function remoteWorkspaces(alias: string): Promise<Workspace[]> {
  const t = tauri();
  if (t === null) throw new Error("not running in the native shell");
  return t.core.invoke<Workspace[]>("remote_workspaces", { alias });
}

/** Subscribe to connect progress events. Returns an unsubscribe promise. */
export function onConnectProgress(
  handler: (p: ConnectProgress) => void,
): Promise<() => void> {
  const t = tauri();
  if (t === null) return Promise.resolve(() => {});
  return t.event.listen<ConnectProgress>("connect-progress", (e) => handler(e.payload));
}

/**
 * Open a workspace in a new window: a real native window in the shell, a
 * new tab in the browser. `alias` null targets the local daemon; a null
 * `wsId` opens the host's home screen (workspace choice happens there).
 */
export async function openWindow(alias: string | null, wsId: string | null): Promise<void> {
  const t = tauri();
  if (t !== null) {
    await t.core.invoke<void>("open_window", { alias, wsId });
    return;
  }
  // Browser: only the local origin is reachable (remote tunnels are the
  // shell's job); compose the fragment the same way `chimaera connect` does.
  const token = getToken();
  const params = new URLSearchParams();
  if (token !== null) params.set("token", token);
  if (wsId !== null) params.set("ws", wsId);
  const hash = params.size > 0 ? `#${params.toString()}` : "";
  window.open(`${location.origin}/${hash}`);
}
