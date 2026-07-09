/**
 * Bridge to the native shell (Tauri), when this UI runs inside it.
 *
 * The shell exposes `window.__TAURI__` (withGlobalTauri) to daemon-served
 * pages, so the web bundle stays shell-agnostic: every helper here has a
 * browser fallback, and `isNativeShell()` gates the shell-only affordances
 * (remote hosts, real windows). Command and event names are the contract
 * with crates/chimaera-app — change them in lockstep.
 */

import { writable } from "svelte/store";
import { getToken } from "./api";
import type { Workspace } from "../workspace/sessions";

interface TauriGlobal {
  core: { invoke: <T>(cmd: string, args?: Record<string, unknown>) => Promise<T> };
  event: {
    listen: <T>(
      event: string,
      handler: (e: { payload: T }) => void,
    ) => Promise<() => void>;
  };
  window: {
    getCurrentWindow: () => {
      close: () => Promise<void>;
      setTitle: (title: string) => Promise<void>;
    };
  };
  webviewWindow: {
    getCurrentWebviewWindow: () => {
      listen: <T>(
        event: string,
        handler: (e: { payload: T }) => void,
      ) => Promise<() => void>;
    };
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
  /**
   * The connected daemon is an older build than this machine's; live
   * sessions kept connect from replacing it (the row offers the update).
   */
  outdated: boolean;
  /** The connected daemon's build id (null = predates build ids). */
  remote_build: string | null;
  /** Live sessions counted when the update decision was made. */
  live_sessions: number | null;
  /**
   * This host connects to its isolated dev daemon (~/.chimaera-dev on the
   * host, running this machine's own build) — never the real ~/.chimaera one.
   */
  dev: boolean;
}

/** Progress of an in-flight connect, mirrored from chimaera-remote phases. */
export interface ConnectProgress {
  alias: string;
  phase: "probing" | "updating" | "downloading" | "installing" | "starting" | "tunneling";
}

/** Build parity of the local daemon, as decided at app startup. */
export interface LocalDaemonState {
  outdated: boolean;
  build: string | null;
  live_sessions: number | null;
}

export async function listHosts(): Promise<HostState[]> {
  const t = tauri();
  if (t === null) return [];
  return t.core.invoke<HostState[]>("list_hosts");
}

/**
 * Save a host. `dev` marks it as a dev-daemon connection: connects deploy
 * THIS machine's build to an isolated ~/.chimaera-dev on the host, leaving
 * the real daemon untouched. The flag persists on the entry (one-way — to
 * leave dev mode, forget the host and re-add it).
 */
export async function addHost(alias: string, dev = false): Promise<HostState> {
  const t = tauri();
  if (t === null) throw new Error("not running in the native shell");
  return t.core.invoke<HostState>("add_host", { alias, dev });
}

export async function removeHost(alias: string): Promise<void> {
  await tauri()?.core.invoke<void>("remove_host", { alias });
}

/**
 * Connect to a saved host (probe, auto-install, start, tunnel). Resolves to
 * the connected state; rejects with the shell's error message on failure.
 * Progress arrives via `onConnectProgress`. `updateDaemon` replaces an
 * outdated remote daemon even when it has live sessions (graceful stop).
 */
export async function connectHost(alias: string, updateDaemon = false): Promise<HostState> {
  const t = tauri();
  if (t === null) throw new Error("not running in the native shell");
  return t.core.invoke<HostState>("connect_host", { alias, updateDaemon });
}

/** The local daemon's build parity (native shell only; null in a browser). */
export async function localDaemonState(): Promise<LocalDaemonState | null> {
  const t = tauri();
  if (t === null) return null;
  return t.core.invoke<LocalDaemonState>("local_state");
}

/**
 * Replace the local daemon with this app's build (graceful stop, respawn).
 * On success the shell broadcasts `local-daemon-updated` and every window
 * on the local daemon re-homes itself — see `onLocalDaemonUpdated`.
 */
export async function updateLocalDaemon(): Promise<void> {
  const t = tauri();
  if (t === null) throw new Error("not running in the native shell");
  await t.core.invoke<void>("update_local_daemon");
}

/**
 * The local daemon was replaced: new port + token, old origin gone. Windows
 * on the local daemon navigate themselves to the new one.
 */
export function onLocalDaemonUpdated(
  handler: (p: { port: number; token: string }) => void,
): Promise<() => void> {
  const t = tauri();
  if (t === null) return Promise.resolve(() => {});
  return t.event.listen<{ port: number; token: string }>("local-daemon-updated", (e) =>
    handler(e.payload),
  );
}

/**
 * Check GitHub releases for a newer signed app build (native shell only).
 * Returns the new version string, or null when up to date / not in the app.
 * The download+verify happens entirely in the Rust shell.
 */
export async function checkAppUpdate(): Promise<string | null> {
  const t = tauri();
  if (t === null) return null;
  return t.core.invoke<string | null>("check_app_update");
}

/**
 * The one-click update chain: install the signed app update and relaunch;
 * the new process finishes by updating the local daemon (sessions resurrect
 * via the daemon's ledger, windows reopen from the shell's registry). Does
 * not return on success.
 */
export async function beginUpdate(): Promise<void> {
  const t = tauri();
  if (t === null) throw new Error("not running in the native shell");
  await t.core.invoke<void>("begin_update");
}

/** This app binary's build id (null in a browser), for skew detection
 *  against the daemon's `/health` build. */
export async function shellBuild(): Promise<string | null> {
  const t = tauri();
  if (t === null) return null;
  return t.core.invoke<string>("shell_build");
}

/**
 * A newer signed app build exists (the shell's periodic updater check).
 * Broadcast to every window; presentation and snoozing are the UI's job.
 */
export function onAppUpdate(handler: (version: string) => void): Promise<() => void> {
  const t = tauri();
  if (t === null) return Promise.resolve(() => {});
  return t.event.listen<string>("app-update", (e) => handler(e.payload));
}

export async function disconnectHost(alias: string): Promise<void> {
  await tauri()?.core.invoke<void>("disconnect_host", { alias });
}

/**
 * End every session on a connected host; its daemon and the tunnel stay up.
 * "Kill everything running here" without the teardown — reconnect not needed.
 */
export async function endHostSessions(alias: string): Promise<void> {
  const t = tauri();
  if (t === null) throw new Error("not running in the native shell");
  await t.core.invoke<void>("end_host_sessions", { alias });
}

/**
 * Shut a connected host down: end every session AND stop its daemon, then drop
 * the tunnel. The real off switch (disconnect leaves the daemon running);
 * reconnecting later starts a fresh daemon.
 */
export async function shutdownHost(alias: string): Promise<void> {
  const t = tauri();
  if (t === null) throw new Error("not running in the native shell");
  await t.core.invoke<void>("shutdown_host", { alias });
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

/** Live tunnel liveness pushed by the shell (health monitor + reconnect). */
export interface HostStatusEvent {
  alias: string;
  /** "down" = the forward stopped answering (remote daemon or ssh died);
   *  "error" = a connect attempt failed (whoever started it). */
  status: "connected" | "down" | "error";
  /** Local end of the tunnel (may change across a reconnect). */
  local_port: number | null;
  /** New daemon token, on "connected" only — lets a window re-home if the
   *  remote daemon restarted. Absent on "down"/"error". */
  token?: string;
  /** The connect failure, on "error" only — so a home screen that merely
   *  observed the attempt (startup restore) can surface it instead of
   *  showing "connecting" forever. */
  error?: string;
}

/**
 * Subscribe to tunnel liveness transitions. Broadcast to every window, so a
 * handler filters on its own host alias. A remote window uses `down` to arm
 * its reconnect overlay and `connected` to re-home when the port/token moved;
 * the home screen uses it to keep host rows live. Returns an unsubscribe.
 */
export function onHostStatus(
  handler: (e: HostStatusEvent) => void,
): Promise<() => void> {
  const t = tauri();
  if (t === null) return Promise.resolve(() => {});
  return t.event.listen<HostStatusEvent>("host-status", (e) => handler(e.payload));
}

/**
 * Tell the shell what this window now shows so focus-existing can raise it
 * later (the SPA swaps `ws` client-side, invisible to the shell otherwise).
 * `alias` null = the local daemon. No-op in a browser.
 */
export async function reportWindowScope(
  alias: string | null,
  ws: string | null,
): Promise<void> {
  await tauri()?.core.invoke<void>("report_window_scope", { alias, ws });
}

/**
 * An SSH auth prompt ssh raised while connecting (no tty in the app, so it
 * comes to us via SSH_ASKPASS). `prompt` is ssh's own text — a password ask,
 * or a keyboard-interactive challenge like a Duo passcode/option menu.
 */
export interface AskpassPrompt {
  id: number;
  prompt: string;
}

/** Subscribe to SSH auth prompts. Returns an unsubscribe promise. */
export function onAskpass(handler: (p: AskpassPrompt) => void): Promise<() => void> {
  const t = tauri();
  if (t === null) return Promise.resolve(() => {});
  return t.event.listen<AskpassPrompt>("ssh-askpass", (e) => handler(e.payload));
}

/**
 * SSH prompts already waiting when this window mounted. The `ssh-askpass`
 * event only reaches windows that exist at emit time — startup window
 * restore starts connecting before the first webview loads, so without this
 * fetch that prompt would be lost and the host stuck "connecting" with
 * nothing to answer.
 */
export async function listAskpass(): Promise<AskpassPrompt[]> {
  const t = tauri();
  if (t === null) return [];
  return t.core.invoke<AskpassPrompt[]>("list_askpass");
}

/**
 * Prompt `id` was resolved somewhere else (answered in another window, or it
 * timed out) — dismiss it here too.
 */
export function onAskpassDone(handler: (id: number) => void): Promise<() => void> {
  const t = tauri();
  if (t === null) return Promise.resolve(() => {});
  return t.event.listen<number>("ssh-askpass-done", (e) => handler(e.payload));
}

/**
 * Whether an SSH auth prompt is currently on screen (set by AskpassModal).
 * The reconnect overlay reads it to say "waiting for authentication" instead
 * of showing a competing spinner/error under the prompt.
 */
export const askpassActive = writable(false);

/**
 * Answer prompt `id`. `secret` null cancels it, letting the waiting ssh fail
 * cleanly instead of hanging.
 */
export async function answerAskpass(id: number, secret: string | null): Promise<void> {
  await tauri()?.core.invoke<void>("answer_askpass", { id, secret });
}

/**
 * Native menu actions forwarded to THIS window ("close-view",
 * "new-terminal", "new-agent"). Window-scoped on purpose: the shell emits
 * to the focused window's label, and a window-scoped listener is what
 * receives targeted events. No-op unsubscriber in the browser.
 */
export function onMenu(handler: (action: string) => void): Promise<() => void> {
  const t = tauri();
  if (t === null) return Promise.resolve(() => {});
  return t.webviewWindow
    .getCurrentWebviewWindow()
    .listen<string>("menu", (e) => handler(e.payload));
}

/** Close this native window (menu Cmd+W on a home window). */
export function closeThisWindow(): void {
  void tauri()?.window.getCurrentWindow().close();
}

/**
 * Set this native window's OS titlebar. The webview does NOT mirror
 * document.title to the native title, so the SPA pushes it here (workspace +
 * host) as the scope changes. No-op in a browser — the tab uses document.title.
 */
export function setNativeWindowTitle(title: string): void {
  void tauri()?.window.getCurrentWindow().setTitle(title);
}

/**
 * Open a workspace window: a real native window in the shell, a new tab in the
 * browser. `alias` null targets the local daemon; a null `wsId` opens the
 * host's home screen (workspace choice happens there). Unless `newWindow`, an
 * existing window already showing this `(alias, wsId)` is raised instead of
 * duplicated.
 */
export async function openWindow(
  alias: string | null,
  wsId: string | null,
  newWindow = false,
): Promise<void> {
  const t = tauri();
  if (t !== null) {
    await t.core.invoke<void>("open_window", { alias, wsId, newWindow });
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
