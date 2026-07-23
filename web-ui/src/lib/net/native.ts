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
import { getHostLabel, getJobContext, getToken } from "./api";
import type { Workspace } from "../workspace/sessions";
import type { ComputeSessionList } from "../workspace/computeSessions";

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
  /**
   * The shell is a dev build (never release-stamped): every connection it
   * makes targets the isolated ~/.chimaera-dev homes on both ends — dev-ness
   * is the build's property, never a per-host choice. Drives the dev badges.
   */
  dev_build: boolean;
}

export async function listHosts(): Promise<HostState[]> {
  const t = tauri();
  if (t === null) return [];
  return t.core.invoke<HostState[]>("list_hosts");
}

/**
 * Save a host. Which home a connect targets (real ~/.chimaera vs the
 * isolated ~/.chimaera-dev) is the BUILD's property, not the host's — a dev
 * build always connects dev, so there is nothing dev-related to pass here.
 */
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
  handler: (p: { port: number; token: string; build?: string }) => void,
): Promise<() => void> {
  const t = tauri();
  if (t === null) return Promise.resolve(() => {});
  return t.event.listen<{ port: number; token: string; build?: string }>("local-daemon-updated", (e) =>
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
 * Write text to the OS clipboard through the native shell. WKWebView rejects
 * `navigator.clipboard.writeText` from a non-gesture callback (an agent's OSC 52,
 * copy-on-select) with NotAllowedError, so on a remote (app-only) window those
 * writes silently failed; the shell writes from the Rust process, which has no
 * transient-activation gate. Returns true when the native write happened; false
 * in a plain browser (or on shell error) so the caller can fall back to
 * `navigator.clipboard`.
 */
export async function writeClipboard(text: string): Promise<boolean> {
  const t = tauri();
  if (t === null) return false;
  try {
    await t.core.invoke<void>("write_clipboard", { text });
    return true;
  } catch {
    return false;
  }
}

/**
 * Hand a web URL to the user's real browser through the native shell.
 *
 * In the app there is no other route: the window's navigation guard admits
 * only the daemon origin, and a `target="_blank"` new-window request has
 * nothing wired to receive it — so an external link was silently swallowed
 * (found live). Returns true when the shell took it; false in a plain browser
 * (or on shell error, e.g. a refused non-http scheme) so the caller can fall
 * back to `window.open`. The shell re-validates the scheme regardless of what
 * we send.
 */
export async function openExternal(url: string): Promise<boolean> {
  const t = tauri();
  if (t === null) return false;
  try {
    await t.core.invoke<void>("open_external", { url });
    return true;
  } catch {
    return false;
  }
}

/**
 * Arm/disarm the "caffeinate" power assertion on the local app host — while on,
 * the machine won't idle/display/system-sleep (incl. lid-closed on macOS, but
 * only on AC power). Global to the app; the change broadcasts (see
 * {@link onCaffeinateChanged}). Returns the resulting armed state; a plain
 * browser (no shell) is a no-op that reports false.
 */
export async function setCaffeinate(on: boolean): Promise<boolean> {
  const t = tauri();
  if (t === null) return false;
  return t.core.invoke<boolean>("set_caffeinate", { on });
}

/** Whether the caffeinate assertion is currently held (read on mount). */
export async function caffeinateState(): Promise<boolean> {
  const t = tauri();
  if (t === null) return false;
  return t.core.invoke<boolean>("caffeinate_state");
}

/** The caffeinate state changed (from any window) — keep this window in sync. */
export function onCaffeinateChanged(handler: (on: boolean) => void): Promise<() => void> {
  const t = tauri();
  if (t === null) return Promise.resolve(() => {});
  return t.event.listen<boolean>("caffeinate-changed", (e) => handler(e.payload));
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

/**
 * The connected host's compute-node sessions + partitions, shell-proxied —
 * the LOCAL home screen's per-host indicator count. The shell strips each
 * session's daemon port/token before anything reaches JS. Managing sessions
 * (launch/cancel/refresh) lives on the host-detail page, which talks to the
 * login daemon's routes directly (see workspace/computeSessions.ts).
 */
export async function remoteComputeSessions(alias: string): Promise<ComputeSessionList> {
  const t = tauri();
  if (t === null) throw new Error("not running in the native shell");
  return t.core.invoke<ComputeSessionList>("remote_compute_sessions", { alias });
}

/** Tunnel to a ready compute-node session; the shell opens its window. */
export async function connectComputeSession(alias: string, jobId: string): Promise<void> {
  const t = tauri();
  if (t === null) throw new Error("not running in the native shell");
  await t.core.invoke<void>("connect_compute_session", { alias, jobId });
}

/**
 * scancel a compute-node session through the LOGIN daemon (`alias` is the
 * login host) and close any live tunnel to that job. A job window ending its
 * own allocation must use this rather than its own daemon's DELETE route:
 * the login daemon owns the launch record (the job daemon has a different
 * compute root, so cancelling there never marks it), and the shell tears the
 * job tunnel down with it. Rejects with the shell's error message on failure.
 */
export async function cancelComputeSession(alias: string, jobId: string): Promise<void> {
  const t = tauri();
  if (t === null) throw new Error("not running in the native shell");
  await t.core.invoke<void>("cancel_compute_session", { alias, jobId });
}

/** Subscribe to connect progress events. Returns an unsubscribe promise. */
export function onConnectProgress(
  handler: (p: ConnectProgress) => void,
): Promise<() => void> {
  const t = tauri();
  if (t === null) return Promise.resolve(() => {});
  return t.event.listen<ConnectProgress>("connect-progress", (e) => handler(e.payload));
}

/** Live tunnel identity pushed by the shell (health monitor + every successful
 *  connect, including reuse of an existing healthy tunnel). */
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
  /** Why a live connection transitioned down. This is context for the
   *  automatic reconnect, not a failed reconnect attempt. */
  reason?: string;
  /** Source build now served through this tunnel. */
  build?: string;
}

/**
 * Subscribe to tunnel liveness transitions. Broadcast to every window, so a
 * handler filters on its own host alias. A remote window uses `down` to arm
 * its reconnect UI and `connected` to re-home when the port/token moved;
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
  label: string | null,
): Promise<void> {
  await tauri()?.core.invoke<void>("report_window_scope", { alias, ws, label });
}

/**
 * An SSH auth prompt ssh raised while connecting (no tty in the app, so it
 * comes to us via SSH_ASKPASS). `prompt` is ssh's own text — a password ask,
 * or a keyboard-interactive challenge like a Duo passcode/option menu.
 */
export interface AskpassPrompt {
  id: number;
  /** SSH alias of the child that raised this prompt. Null only for legacy or
   *  unscoped helpers, which remain available from the home window. */
  alias?: string | null;
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
 * The reconnect UI hides while its matching auth prompt owns the interaction,
 * instead of showing a competing status or error beneath the modal.
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
 * Set this native window's OS title. The webview does NOT mirror document.title
 * to the native title, so the SPA pushes it here (workspace + host) as the scope
 * changes. macOS overlay windows keep it as hidden system metadata; browser
 * tabs render document.title normally.
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
  // `window.open` clones the opener's sessionStorage into the new browsing
  // context. Without an explicit fresh id, both tabs therefore persist into
  // the same daemon-side view-state key and overwrite each other's layouts.
  params.set("win", `w-${crypto.randomUUID()}`);
  const host = getHostLabel();
  if (host !== "local") params.set("host", host);
  const job = getJobContext();
  if (job !== null) {
    params.set("job", job.jobId);
    if (job.node !== null) params.set("node", job.node);
  }
  const hash = params.size > 0 ? `#${params.toString()}` : "";
  // The hash now carries every piece of per-window state, so severing opener
  // access is safe and avoids coupling the two app tabs.
  window.open(`${location.origin}/${hash}`, "_blank", "noopener");
}
