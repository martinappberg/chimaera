/**
 * Bridge to the native shell (Tauri), when this UI runs inside it.
 *
 * The shell exposes `window.__TAURI__` (withGlobalTauri) to daemon-served
 * pages, so the web bundle stays shell-agnostic: every helper here has a
 * browser fallback, and `isNativeShell()` gates the shell-only affordances
 * (remote hosts, real windows). Command and event names are the contract
 * with crates/chimaera-app — change them in lockstep.
 */

import { workbenchPath } from "./base";
import { writable } from "svelte/store";
import { getHostLabel, getJobContext, getToken } from "./api";
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
export type HostStatus = "disconnected" | "connecting" | "connected" | "cluster" | "error";

/** A batch scheduler a host's login shell reaches (only Slurm is driven). */
export type Scheduler = "slurm" | "pbs" | "lsf";

/**
 * A host where a batch scheduler was found: chimaera never runs on its login
 * node (unless `login_serve`), workspaces run as jobs instead.
 */
export interface ClusterHostInfo {
  scheduler: Scheduler;
  /** The user allowed a daemon on this cluster's login node (warned). */
  login_serve: boolean;
  /** A daemon an earlier connect left on the login node, if the probe saw it. */
  login_daemon: { node: string; pid: number; alive: boolean | null } | null;
}

export interface HostState {
  alias: string;
  /** Optional when attached to a shell predating the link transport. */
  via_pro?: boolean;
  kept?: boolean;
  /** Local SSH choice; absent for devices and shells without this command. */
  direct_ssh?: boolean;
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
   * The login node the daemon runs on when the alias names a pool of login
   * nodes and the connection is pinned to one other than where a new ssh
   * connection lands (null = wherever the alias lands).
   */
  node: string | null;
  /**
   * Set when the host is a cluster — from this session's connect, or the
   * scheduler the last connect found (a hint until the next probe).
   */
  cluster: ClusterHostInfo | null;
  /** The user said this host isn't a cluster, though Slurm is on its PATH:
   *  it connects like any remote (its row offers to undo this). */
  not_cluster: boolean;
  /** Absent on older native shells. */
  cluster_setup_complete?: boolean;
}

/** Progress of an in-flight connect, mirrored from chimaera-remote phases. */
export interface ConnectProgress {
  alias: string;
  phase:
    | "probing"
    | "routing"
    | "updating"
    | "downloading"
    | "installing"
    | "starting"
    | "tunneling"
    /** A cluster's binary install finished (or failed): its row's line goes.
     *  (A connect ends with its `host-status` instead.) */
    | "done";
  /** The login node a `routing` phase is reaching (it may ask to authenticate). */
  node?: string;
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

/** Bounded first-paint palette persisted by the native shell per host. */
export interface AppearanceBootstrap {
  mode: "light" | "dark";
  themeId: string;
  background: string;
  accent: string | null;
}

/**
 * Keep the confirmed palette outside origin-scoped browser storage. Native
 * daemon/tunnel ports are volatile, so the shell carries this snapshot into
 * the next URL before its document is parsed.
 */
export async function cacheAppearanceBootstrap(
  appearance: AppearanceBootstrap,
): Promise<void> {
  await tauri()?.core.invoke<void>("cache_appearance", { appearance });
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

/** The shell's signed-update knowledge (`app_update_status`). */
export interface AppUpdateStatus {
  /** This app's own version. */
  current: string;
  /** A dev build never checks (an "update" would swap the build under test). */
  dev: boolean;
  /** Last check attempt, unix seconds (null = never). */
  checked_at: number | null;
  /** A newer signed version, when the last good answer had one. */
  available: string | null;
  /** Why the last attempt failed; null after a success. */
  error: string | null;
  interval_secs: number;
}

/**
 * The shell's answer to "is there an app update?" — cached instantly, or
 * `refresh` to check first. Null in a browser.
 */
export async function appUpdateStatus(refresh: boolean): Promise<AppUpdateStatus | null> {
  const t = tauri();
  if (t === null) return null;
  return t.core.invoke<AppUpdateStatus>("app_update_status", { refresh });
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
 * Whether this window can hand the daemon's files to the OS by path: the
 * native shell, on the machine the files live on. A remote window's paths
 * name another machine's files, and on Windows the local daemon runs inside
 * WSL2. The shell enforces the same rule; this only decides what to offer.
 */
export function hasLocalFiles(): boolean {
  return tauri() !== null && getHostLabel() === "local" && !/\bWindows\b/.test(navigator.userAgent);
}

/** What the platform calls showing a file in its file manager. */
export function revealLabel(): string {
  return /\bMac/.test(navigator.userAgent) ? "Reveal in Finder" : "Show in File Manager";
}

/**
 * The "Reveal in Finder" row for a files menu — empty where this window's
 * files are not this machine's, so a menu spreads it unconditionally.
 */
export function revealEntries(path: string): { label: string; onSelect: () => void }[] {
  return hasLocalFiles() ? [{ label: revealLabel(), onSelect: () => void revealInFileManager(path) }] : [];
}

/**
 * Put a file or folder itself on the OS clipboard (what copying it in the
 * file manager does), through the native shell. True when it is there; false
 * in a browser, on a remote window, or on shell error.
 */
export async function copyFileToClipboard(path: string): Promise<boolean> {
  const t = tauri();
  if (t === null || !hasLocalFiles()) return false;
  try {
    await t.core.invoke<void>("copy_file_to_clipboard", { path });
    return true;
  } catch {
    return false;
  }
}

/** Show a file or folder in the OS file manager. Only where `hasLocalFiles()`. */
export async function revealInFileManager(path: string): Promise<boolean> {
  const t = tauri();
  if (t === null || !hasLocalFiles()) return false;
  try {
    await t.core.invoke<void>("reveal_in_file_manager", { path });
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

// --- Clusters: jobs, and workspaces open inside them ----------------------
//
// On a cluster nothing of ours keeps running on the login node: the shell
// runs short ssh commands (squeue/sbatch/scancel + the read-only `chimaera
// browse`), and Chimaera runs inside Slurm jobs the user starts. Workspaces
// open inside a job; a workspace keeps its chats in its own folder, so it
// moves between jobs. Ports and tokens never reach JS.

/** What a job asks Slurm for; absent fields = the cluster's default. */
export interface LaunchSpec {
  /** Slurm time limit (`2-00:00:00`, `04:00:00`). Required. */
  time: string;
  partition?: string | null;
  account?: string | null;
  qos?: string | null;
  constraint?: string | null;
  cpus?: number | null;
  /** Memory per node, Slurm's spelling (`16G`). */
  mem?: string | null;
  gpus?: number | null;
}

export interface ClusterWorkspace {
  id: string;
  name: string;
  path: string;
  created_ms: number;
}

export interface ClusterSetup {
  name: string;
  spec: LaunchSpec;
}

export interface AgentRules {
  /** A file on the cluster holding its published rules for agents. */
  file?: string | null;
  /** Text the user wrote or pasted. */
  text: string;
}

export interface ClusterConfig {
  version: number;
  workspaces: ClusterWorkspace[];
  setups: ClusterSetup[];
  /** What the last job on this cluster started with. */
  last_spec?: LaunchSpec | null;
  agent_rules: AgentRules;
  learned: {
    /** Partitions that refused a batch job (started attached instead). */
    interactive_only?: string[];
    /** Fields the cluster insisted on: "account" | "qos" | "constraint". */
    requires?: string[];
  };
}

export type ClusterJobState = "waiting" | "starting" | "running" | "ended";

export interface ClusterJob {
  id: string;
  /** What the page calls it: a saved setup's name, else partition · time. */
  name: string;
  state: ClusterJobState;
  slurm_job_id?: string;
  node?: string;
  partition?: string;
  cpus?: string;
  mem?: string;
  gpus?: number;
  /** When its time runs out (epoch ms, cluster clock). */
  ends_at_ms?: number;
  /** Why it waits, when it isn't plain priority (Slurm's own word). */
  reason?: string;
  /** Held in the foreground by this app's connection (stops on disconnect). */
  attached: boolean;
  /** How it ended (Slurm's terminal state: TIMEOUT, CANCELLED, …). */
  ended?: string;
  ended_at_ms?: number;
  stopped_by_user: boolean;
  /** Cancellation accepted or Slurm is completing; absent on older hosts. */
  stopping?: boolean;
  /** Its node reaches the internet (agents can work), once probed. */
  egress?: boolean;
  /** Workspaces it opens when it starts. */
  open: string[];
  /** The job it continues. */
  replaces?: string;
  spec: LaunchSpec;
  /** Its own startup commands. */
  startup: string;
  submitted_ms: number;
  /** Slurm's start estimate while it waits (epoch ms), when it has one. */
  start_estimate_ms?: number | null;
}

export type ClusterWorkspaceState = "open" | "queued" | "closed";

export interface ClusterWorkspaceView {
  id: string;
  name: string;
  path: string;
  /** `open` in `job`, `queued` (opens when `job` starts), or `closed`. */
  state: ClusterWorkspaceState;
  job?: string;
  last_open_ms?: number;
  /** Agents working right now, when the app holds its job's connection. */
  working?: number;
  /** Its chimaera exited on its own — the last lines it printed. */
  failed?: string;
  /** Its job is opening it right now (it may be waiting for another job to let go). */
  opening?: boolean;
  /** It is saving its chats and closing (a close or a move). */
  closing?: boolean;
}

export interface ClusterOverview {
  scheduler: Scheduler;
  login_node: string;
  /** The user's home folder on the cluster (paths under it show as `~/…`). */
  home?: string;
  now_ms: number;
  jobs: ClusterJob[];
  workspaces: ClusterWorkspaceView[];
  /** The user's other (non-chimaera) Slurm jobs — a count, no controls. */
  other_jobs: { running: number; waiting: number };
  /** The queue couldn't be read this round; states are carried forward. */
  degraded: boolean;
  /** When the queue was last actually asked (epoch ms). */
  queue_at_ms: number;
  config: ClusterConfig;
  /** Startup commands: the cluster's, and each workspace's (by id). */
  startup: { cluster: string; workspaces: Record<string, string> };
}

export interface PartitionChoice {
  name: string;
  default: boolean;
  /** sinfo's limit, raw (`7-00:00:00`, `UNLIMITED`) and in seconds. */
  max_time: string;
  max_time_secs: number | null;
  cpus_per_node: string;
  mem_per_node: string;
  gpus: boolean;
  preemptible: boolean;
  up: boolean;
  /** Accounts that may submit here (empty when the cluster uses none). */
  accounts?: string[];
}

export interface ClusterFacts {
  scheduler: Scheduler;
  version: string;
  partitions: PartitionChoice[];
  accounts: string[];
  default_account: string | null;
  fetched_ms: number;
}

export type StartResult =
  | { kind: "submitted"; job: string; slurm_job_id: string }
  | { kind: "attached"; job: string }
  | {
      kind: "refused";
      /** The scheduler's own words, cleaned — show verbatim. */
      message: string;
      refusal:
        | "batch_not_allowed"
        | "account_required"
        | "qos_required"
        | "constraint_required"
        | "other";
    };

/** One folder's subfolders, for choosing a workspace. */
export interface ClusterDirListing {
  path: string;
  parent?: string | null;
  /** The folder itself is already a workspace: its id. */
  workspace?: string | null;
  folders: { name: string; git: boolean; workspace?: string | null }[];
  truncated: boolean;
}

function shell(): TauriGlobal {
  const t = tauri();
  if (t === null) throw new Error("not running in the native shell");
  return t;
}

/**
 * The cluster page's data in one ssh exec. The queue itself is asked at most
 * once a minute per host (a cached read fills in between).
 */
export async function clusterOverview(alias: string, refresh = false): Promise<ClusterOverview> {
  return shell().core.invoke<ClusterOverview>("cluster_overview", { alias, refresh });
}

/** Partitions, limits and accounts the start sheet offers (cached a day). */
export async function clusterFacts(alias: string, refresh = false): Promise<ClusterFacts> {
  return shell().core.invoke<ClusterFacts>("cluster_facts", { alias, refresh });
}

/** Add a folder on the cluster as a workspace. */
export async function clusterAddWorkspace(
  alias: string,
  path: string,
  name: string,
): Promise<ClusterWorkspace> {
  return shell().core.invoke<ClusterWorkspace>("cluster_add_workspace", { alias, path, name });
}

/** Take a closed workspace off the list (its folder and chats stay put). */
export async function clusterRemoveWorkspace(alias: string, workspaceId: string): Promise<void> {
  await shell().core.invoke<void>("cluster_remove_workspace", { alias, workspaceId });
}

/** One folder's subfolders on the cluster (`~` and `$VARS` expand there). */
export async function clusterListDir(alias: string, path: string): Promise<ClusterDirListing> {
  return shell().core.invoke<ClusterDirListing>("cluster_list_dir", { alias, path });
}

/**
 * Start a job. `open` = workspaces to open when it starts; `runStartup` =
 * startup commands for this job only; `saveAs` names a setup to remember
 * (and names the job). A partition that refuses batch jobs comes back
 * `refused` with `batch_not_allowed` — retry with `attached`.
 */
export async function clusterStartJob(
  alias: string,
  spec: LaunchSpec,
  open: string[],
  runStartup: string,
  name: string | null,
  saveAs: string | null,
  attached: boolean,
): Promise<StartResult> {
  return shell().core.invoke<StartResult>("cluster_start_job", {
    alias,
    spec,
    open,
    runStartup,
    name,
    saveAs,
    attached,
  });
}

/**
 * Continue a running job in a new one (named by `jobId`, or by a workspace
 * open in it): the same setup unless `spec` says otherwise. When the new job
 * starts, it takes this job's workspaces over and this one stops.
 */
export async function clusterContinueJob(
  alias: string,
  target: { jobId: string } | { workspaceId: string },
  spec: LaunchSpec | null = null,
  runStartup: string | null = null,
): Promise<StartResult> {
  return shell().core.invoke<StartResult>("cluster_continue_job", {
    alias,
    jobId: "jobId" in target ? target.jobId : null,
    workspaceId: "workspaceId" in target ? target.workspaceId : null,
    spec,
    runStartup,
  });
}

/** Stop a job (every workspace in it saves its chats first). */
export async function clusterStopJob(alias: string, jobId: string): Promise<void> {
  await shell().core.invoke<void>("cluster_stop_job", { alias, jobId });
}

/** Forget an ended job's line. */
export async function clusterDismissJob(alias: string, jobId: string): Promise<void> {
  await shell().core.invoke<void>("cluster_dismiss_job", { alias, jobId });
}

/**
 * Open a workspace's window: where it's open, else in job `jobId` (which the
 * page chose). Resolves once the window is up.
 */
export async function clusterOpen(
  alias: string,
  workspaceId: string,
  jobId: string | null = null,
): Promise<void> {
  await shell().core.invoke<void>("cluster_open", { alias, workspaceId, jobId });
}

/** Close a workspace in its job (it saves its chats). */
export async function clusterClose(alias: string, workspaceId: string): Promise<void> {
  await shell().core.invoke<void>("cluster_close", { alias, workspaceId });
}

/** Move an open workspace to another running job; its chats and windows follow. */
export async function clusterMove(alias: string, workspaceId: string, jobId: string): Promise<void> {
  await shell().core.invoke<void>("cluster_move", { alias, workspaceId, jobId });
}

/** Open a workspace when a job that is still waiting starts. */
export async function clusterQueueOpen(alias: string, workspaceId: string, jobId: string): Promise<void> {
  await shell().core.invoke<void>("cluster_queue_open", { alias, workspaceId, jobId });
}

/** Save startup commands: the cluster's (`workspaceId` null) or one workspace's. */
export async function clusterSetStartup(
  alias: string,
  workspaceId: string | null,
  text: string,
): Promise<void> {
  await shell().core.invoke<void>("cluster_set_startup", { alias, workspaceId, text });
}

/** Forget a saved setup. */
export async function clusterForgetSetup(alias: string, name: string): Promise<void> {
  await shell().core.invoke<void>("cluster_forget_setup", { alias, name });
}

/** Save what agents on this cluster are told about its rules. */
export async function clusterSetAgentRules(alias: string, rules: AgentRules): Promise<void> {
  await shell().core.invoke<void>("cluster_set_agent_rules", { alias, rules });
}

/** The warned per-host override: allow chimaera on this cluster's login node. */
export async function clusterSetLoginServe(alias: string, on: boolean): Promise<HostState> {
  return shell().core.invoke<HostState>("cluster_set_login_serve", { alias, on });
}

/** Say a host isn't a cluster after all (`on`), or is one again. */
export async function setNotCluster(alias: string, on: boolean): Promise<HostState> {
  return shell().core.invoke<HostState>("set_not_cluster", { alias, on });
}

/** Choose direct SSH on this computer's next connection, keeping existing work. */
export async function setHostDirectSsh(alias: string, on: boolean): Promise<HostState> {
  return shell().core.invoke<HostState>("set_host_direct_ssh", { alias, on });
}

/** SIGTERM a daemon an earlier connect left on the cluster's login node. */
export async function clusterStopLoginDaemon(alias: string): Promise<void> {
  await shell().core.invoke<void>("cluster_stop_login_daemon", { alias });
}

/** Open a terminal-only window with `ssh <alias>` (the login node). */
export async function clusterOpenTerminal(alias: string): Promise<void> {
  await shell().core.invoke<void>("cluster_open_terminal", { alias });
}

/** A cluster's jobs or workspaces changed — refetch. */
export function onClusterChanged(handler: (alias: string) => void): Promise<() => void> {
  const t = tauri();
  if (t === null) return Promise.resolve(() => {});
  return t.event.listen<{ alias: string }>("cluster-changed", (e) => handler(e.payload.alias));
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
   *  "error" = a connect attempt failed (whoever started it);
   *  "ended" = a job window's job left the queue (its composite
   *  `<alias>#job<id>` key only) — `reason` then carries Slurm's state
   *  (TIMEOUT, CANCELLED, FAILED, PREEMPTED, …) or "stopped" when the user
   *  stopped it. A "down" without "ended" is still just a connection blip. */
  status: "connected" | "down" | "error" | "ended";
  /** Local end of the tunnel (may change across a reconnect). */
  local_port: number | null;
  /** New daemon token, on "connected" only — lets a window re-home if the
   *  remote daemon restarted. Absent on "down"/"error". */
  token?: string;
  /** The connect failure, on "error" only — so a home screen that merely
   *  observed the attempt (startup restore) can surface it instead of
   *  showing "connecting" forever. */
  error?: string;
  /** Why a live connection transitioned down (context for the automatic
   *  reconnect, not a failed attempt) — or, on "ended", how the job ended. */
  reason?: string;
  /** Source build now served through this tunnel. */
  build?: string;
  /** On "connected": the login node the tunnel is pinned to (absent = wherever
   *  the alias lands). Every connected event carries it. */
  node?: string;
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
  detached = false,
): Promise<void> {
  // `detached` is set-only shell-side: true re-asserts a restored detached
  // window's flag (its blob carries dt:1); false never clears anything.
  await tauri()?.core.invoke<void>("report_window_scope", { alias, ws, label, detached });
}

/**
 * Tell the shell which sessions this window has on screen (each pane's
 * active tab). The shell drops a notification about one of them while this
 * window has focus — the user is already looking — and clears their
 * delivered alerts. No-op in a browser.
 */
export async function reportWindowView(visible: string[]): Promise<void> {
  await tauri()?.core.invoke<void>("report_window_view", { visible });
}

/**
 * Tell the shell how many files hold unsaved edits in this window, whenever
 * that changes. The shell decides a window close or the app's quit from this
 * count without asking the page first, so a window with nothing unsaved
 * closes with no prompt. No-op in a browser (beforeunload guards there).
 */
export async function reportUnsaved(count: number): Promise<void> {
  await tauri()?.core.invoke<void>("report_unsaved", { count });
}

/** Why the shell is asking this window about its unsaved edits. */
export type UnsavedReason = "close" | "quit";

/**
 * The shell held this window's close, or the app's quit, because this window
 * reported unsaved edits. `id` is stable across repeated asks of one prompt;
 * every reply carries it.
 */
export interface UnsavedPrompt {
  id: number;
  reason: UnsavedReason;
}

/**
 * - `shown`: the dialog is up — the page is alive, so the shell waits for the
 *   user instead of treating the window as hung (it proceeds anyway after a
 *   few seconds without this).
 * - `proceed`: every file saved, or Don't save — the close or quit goes ahead.
 * - `cancel`: keep the window; a quit is abandoned.
 */
export type UnsavedReply = "shown" | "proceed" | "cancel";

/** The shell asks about this window's unsaved edits (window-scoped, like onMenu). */
export function onUnsavedPrompt(handler: (p: UnsavedPrompt) => void): Promise<() => void> {
  const t = tauri();
  if (t === null) return Promise.resolve(() => {});
  return t.webviewWindow
    .getCurrentWebviewWindow()
    .listen<UnsavedPrompt>("unsaved-prompt", (e) => handler(e.payload));
}

export async function replyUnsaved(id: number, reply: UnsavedReply): Promise<void> {
  await tauri()?.core.invoke<void>("reply_unsaved", { id, reply });
}

/**
 * A notification was clicked and this window should show `sessionId`.
 * Window-scoped: the shell emits to the chosen window's label. No-op
 * unsubscriber in the browser.
 */
export function onFocusSession(handler: (sessionId: string) => void): Promise<() => void> {
  const t = tauri();
  if (t === null) return Promise.resolve(() => {});
  return t.webviewWindow
    .getCurrentWebviewWindow()
    .listen<string>("focus-session", (e) => handler(e.payload));
}

/**
 * The session a notification click opened this window for, if any — asked
 * once the `focus-session` listener is live, in case the shell's event beat
 * it. Null in a browser.
 */
export async function takePendingFocus(): Promise<string | null> {
  const t = tauri();
  if (t === null) return null;
  return (await t.core.invoke<string | null>("take_pending_focus")) ?? null;
}

/** OS notification permission as the shell sees it. */
export type NativeNotificationPermission =
  | "granted"
  | "denied"
  | "not_determined"
  | "unsupported";

export async function notificationPermission(): Promise<NativeNotificationPermission> {
  const t = tauri();
  if (t === null) return "unsupported";
  return t.core.invoke<NativeNotificationPermission>("notification_permission");
}

/** Ask the OS for notification permission now (its one-time prompt). */
export async function requestNotificationPermission(): Promise<NativeNotificationPermission> {
  const t = tauri();
  if (t === null) return "unsupported";
  return t.core.invoke<NativeNotificationPermission>("request_notification_permission");
}

/** Open the OS's notification settings for Chimaera (macOS System Settings). */
export async function openNotificationSettings(): Promise<void> {
  await tauri()?.core.invoke<void>("open_notification_settings");
}

/** Post a sample notification (the settings page's "Send test"). */
export async function testNotification(): Promise<void> {
  await tauri()?.core.invoke<void>("test_notification");
}

/**
 * An SSH auth prompt ssh raised while connecting (no tty in the app, so it
 * comes to us via SSH_ASKPASS). `prompt` is ssh's own text — a password ask,
 * or a keyboard-interactive challenge like a Duo passcode/option menu.
 */
export interface AskpassPrompt {
  id: number;
  source?: { type: "local" } | { type: "keeper"; host_id: string; keeper_prompt_id: string };
  /** Only the original native Connect verifier emits a host-key confirmation. */
  kind?: { type: "host_key"; host: string; fingerprint: string };
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
    if (job.cws !== null) params.set("cws", job.cws);
  }
  const hash = params.size > 0 ? `#${params.toString()}` : "";
  // The hash now carries every piece of per-window state, so severing opener
  // access is safe and avoids coupling the two app tabs.
  window.open(`${location.origin}${workbenchPath()}${hash}`, "_blank", "noopener");
}

/**
 * Navigate the unused native launcher to local Home, a connected remote detail,
 * or a workspace on that daemon. A workspace consumes the launcher and becomes
 * an ordinary workbench; the next New Window can then create a fresh Home.
 */
export async function navigateHome(alias: string | null, wsId: string | null = null): Promise<void> {
  const t = tauri();
  if (t === null) throw new Error("Home navigation requires the native shell");
  await t.core.invoke<void>("navigate_home", { alias, wsId });
}

/**
 * Native half of pane detach: a real OS window booting on a PRE-SEEDED
 * window id (the caller PUT the `dt:1` solo layout blob under `winId`
 * first). `at` is the drop point in THIS window's client coords plus the
 * desired inner size — the shell lifts them into screen space itself; it
 * never trusts webview screen coordinates. The shell also derives the host
 * from this window's registered scope, so there is no alias parameter.
 */
export async function openDetachedWindow(
  wsId: string,
  winId: string,
  at: { x: number; y: number; w: number; h: number },
): Promise<void> {
  const t = tauri();
  if (t === null) throw new Error("not in the native shell");
  // `at` is one nested argument (the command takes a DetachAt struct) — a
  // flat spread would fail Tauri's per-key argument extraction.
  await t.core.invoke<void>("open_detached_window", {
    wsId,
    winId,
    at: { x: at.x, y: at.y, width: at.w, height: at.h },
  });
}

// --- cross-window drag / adopt (shell-routed; see crossWindow.ts) ----------

/** A sibling window tabs can move to (list_scope_windows / the tray). */
export interface ScopeWindow {
  win_id: string;
  label: string;
  detached: boolean;
}

/** An `xdrag` event targeted at THIS window, coords in its client px. */
export interface XdragEvent {
  phase: "over" | "leave" | "drop";
  x?: number;
  y?: number;
  transfer?: number;
  payload?: unknown;
}

/** Track an out-of-window drag: resolves whether a sibling window is under
 *  the pointer (client coords — the shell owns the screen-space math).
 *  `drag` fences late frames: a track for an already-ended drag is ignored
 *  shell-side, so it can never re-light a target a cancel just cleared.
 *  Rejections degrade to "nothing there" — this runs per animation frame. */
export async function dragTrack(x: number, y: number, drag: number): Promise<boolean> {
  try {
    return (await tauri()?.core.invoke<boolean>("drag_track", { x, y, drag })) ?? false;
  } catch {
    return false;
  }
}

/** Route the release. `transfer` is SENDER-minted (see mintTransfer) so the
 *  ledger is armed before this call — the target's ack can never beat the
 *  entry it resolves against. */
export async function dragDrop(
  x: number,
  y: number,
  drag: number,
  transfer: number,
  payload: unknown,
): Promise<{ routed: boolean }> {
  return (
    (await tauri()?.core.invoke<{ routed: boolean }>("drag_drop", {
      at: { x, y },
      drag,
      transfer,
      payload,
    })) ?? { routed: false }
  );
}

export async function dragCancel(drag: number): Promise<void> {
  await tauri()?.core.invoke<void>("drag_cancel", { drag });
}

export async function adoptAck(transfer: number, ok: boolean): Promise<void> {
  await tauri()?.core.invoke<void>("adopt_ack", { transfer, ok });
}

/** Menu-path adopt into the window with stable id `targetWinId`. The
 *  sender-minted `transfer` was armed in the ledger before this call. */
export async function adoptTab(
  targetWinId: string,
  transfer: number,
  payload: unknown,
): Promise<void> {
  const t = tauri();
  if (t === null) throw new Error("not in the native shell");
  await t.core.invoke<void>("adopt_tab", { targetWinId, transfer, payload });
}

export async function listScopeWindows(): Promise<ScopeWindow[]> {
  return (await tauri()?.core.invoke<ScopeWindow[]>("list_scope_windows", {})) ?? [];
}

/** Incoming cross-window drag traffic for THIS window (window-scoped, like
 *  onMenu). No-op unsubscriber in the browser. */
export function onXdrag(handler: (e: XdragEvent) => void): Promise<() => void> {
  const t = tauri();
  if (t === null) return Promise.resolve(() => {});
  return t.webviewWindow
    .getCurrentWebviewWindow()
    .listen<XdragEvent>("xdrag", (e) => handler(e.payload));
}

/** Acks for transfers THIS window initiated (drag drops and menu adopts). */
export function onXdragAck(
  handler: (e: { transfer: number; ok: boolean }) => void,
): Promise<() => void> {
  const t = tauri();
  if (t === null) return Promise.resolve(() => {});
  return t.webviewWindow
    .getCurrentWebviewWindow()
    .listen<{ transfer: number; ok: boolean }>("xdrag-ack", (e) => handler(e.payload));
}

/**
 * Browser half of pane detach: a popup window booting on a PRE-SEEDED window
 * id (the caller PUT the solo layout blob under `winId` first — same
 * sessionStorage-clone defense as openWindow, the id just isn't fresh-minted
 * here). Position/size are best-effort screen coords from the drop point.
 * Returns false when a popup blocker ate it, so the caller keeps the tabs —
 * which is why this opens WITH an opener (a `noopener` open always returns
 * null and would make a block undetectable); the handle's opener is severed
 * right after, keeping the two windows as uncoupled as openWindow's.
 */
export function openDetachedPopup(
  winId: string,
  wsId: string,
  at: { x: number; y: number; w: number; h: number },
): boolean {
  const token = getToken();
  const params = new URLSearchParams();
  if (token !== null) params.set("token", token);
  params.set("ws", wsId);
  params.set("win", winId);
  // dt=1 is how the child knows it is a solo window BEFORE its layout blob
  // loads (no workspace-mirror fallback), and what triggers its purge of the
  // chat-draft keys this auxiliary context cloned from us.
  params.set("dt", "1");
  const host = getHostLabel();
  if (host !== "local") params.set("host", host);
  const job = getJobContext();
  if (job !== null) {
    params.set("job", job.jobId);
    if (job.node !== null) params.set("node", job.node);
    if (job.cws !== null) params.set("cws", job.cws);
  }
  const features = [
    "popup=yes",
    `width=${Math.round(at.w)}`,
    `height=${Math.round(at.h)}`,
    `left=${Math.round(at.x)}`,
    `top=${Math.round(at.y)}`,
  ].join(",");
  const popup = window.open(`${location.origin}${workbenchPath()}#${params.toString()}`, "_blank", features);
  if (popup === null) return false;
  popup.opener = null;
  return true;
}

export interface ProBillingAttempt {
  id: number;
  kind: "checkout" | "portal" | "plan_change";
  requested_plan?: "pro" | "max" | null;
  phase: "opening" | "waiting" | "confirming" | "confirmed" | "unconfirmed" | "canceled" | "expired" | "failed";
  expires_at: number;
  error: string | null;
}

/** One price from the account service; `amount_cents` is in the currency's minor unit.
 * `cloud_time_multiple` and `storage_multiple` say how many times the plan's
 * monthly cloud time and storage are Pro's, whole numbers (the same pair on
 * both intervals, 1 on Pro's own entries); older services omit them. The list
 * carries no absolute allowance. */
export interface ProPlanPrice {
  plan: "pro" | "max";
  interval: "month" | "year";
  amount_cents: number;
  currency: string;
  cloud_time_multiple?: number | null;
  storage_multiple?: number | null;
}

/** Account controls are native-shell state, separate from daemon settings. */
export interface ProStatus {
  /** Opaque original account-owner lifetime, including signed-out state.
   * Absent on older shells; a new private account surface requires it. */
  account_lifetime?: string | null;
  /** Original signed-out lifetime of a successful explicit native Sign in;
   * only while that resulting generation remains current. Never mutation authority. */
  completed_sign_in_lifetime?: string | null;
  initializing?: boolean;
  initialization_phase?: "keychain" | "account" | "connection" | null;
  available: boolean;
  signed_in: boolean;
  email: string | null;
  plan: "pro" | "max" | "none" | null;
  /** A real account failure. Informational connection progress uses
   * `connection_warning` (older shells sent two such messages here). */
  error: string | null;
  /** Optional: the always-on connection is still coming up. Never a failure
   * and never an entitlement signal. Older shells omit it. */
  connection_warning?: string | null;
  /** Optional: the subscription's payment failed and needs the customer's
   * attention (billing portal). Checkout must never start a second plan. */
  payment_due?: boolean;
  /** Optional: once the plan has ended, the RFC 3339 time until which its cloud
   * work can still be brought home; null or absent otherwise (older shells
   * omit it). An ended plan grants nothing (`pro/status.ts` `grantedPlan`). */
  returning_until?: string | null;
  /** Optional: the RFC 3339 time the always-on cloud connection restarts to
   * update (a past time: shortly, once no Git transfer runs). The restart drops
   * the cluster logins it holds. Null or absent when none is planned (older
   * shells omit it); `pro/status.ts` `keeperRestartAt` reads it. */
  keeper_restart_at?: string | null;
  /** Optional: the account's current prices. Absent or null means the page
   * names the plans only; amounts are never built into the app. */
  plans?: ProPlanPrice[] | null;
  /** Optional when connected to an older native shell. */
  sign_in?: { phase: "waiting" | "finishing"; expires_at: number } | null;
  /** Native owns verification even when this page is closed. Older shells omit it. */
  billing?: ProBillingAttempt | null;
  limits?: { cloud_hours: number; storage_bytes: number } | null;
  usage?: { cloud_hours: number; storage_bytes: number } | null;
  hours_exhausted?: boolean;
}

export interface CloudProvisioningStatus {
  phase?: "keeper" | "worker" | "connecting" | null;
  /** Whether an agent was connected in the cloud at the last catalog read,
   * remembered by the app and never probed, so a sleeping cloud is not woken
   * to answer it. Absent or null when the app has not seen a catalog yet. */
  agents_connected?: boolean | null;
  /** Whether this account's cloud has been ready before, remembered by the
   * app: a later `preparing` (a service update, say) is not first-time setup.
   * Older shells omit it (`presentation.ts` `cloudReadyOnce`). */
  cloud_ready_once?: boolean;
  /** The provider rows of the last catalog read, remembered by the app and
   * shown at once until a live read answers (`providers.ts`
   * `rememberedRows`). Absent when none are remembered. */
  remembered_providers?: RememberedProvider[];
  state: "no_plan" | "unavailable" | "preparing" | "ready" | "sleeping" | "limited" | "error";
  reason: "provisioning_disabled" | "beta_invite_required" | "hours_exhausted" | "storage_exhausted" | "spend_limit_reached" | "provisioning_failed" | null;
  /** Explicit actions allowed after the unattended allowance, only for
   * limited/hours_exhausted. Absence retains older hard-limit behavior. */
  attended_actions?: boolean;
}

/** Omission is compatibility for original callers only. New account surfaces
 * always supply their captured receipt; a rejection never retries omission. */
function accountGuard(expectedAccountLifetime?: string): { expectedAccountLifetime?: string } {
  if (expectedAccountLifetime === undefined) return {};
  if (!/^[0-9a-f]{64}$/.test(expectedAccountLifetime)) throw new Error("account_changed");
  return { expectedAccountLifetime };
}

export interface CloudProject {
  workspace_id: string;
  name: string;
  host_id?: string | null;
  host_alias?: string | null;
  local_root: string | null;
  /** Saved destination binding, including interrupted copies not ready to open. */
  destination_saved?: boolean;
  available: boolean;
  error: string | null;
}

export interface CloudProjectOpen {
  workspace_id: string;
  root: string;
  name: string;
  local_copy?: LocalProjectCopy;
}

export async function proOpenCloudProject(workspaceId: string, expectedAccountLifetime?: string): Promise<CloudProjectOpen | null> {
  const t = tauri();
  if (t === null) throw new Error("Open the desktop app to open this project here.");
  // A distinct command refuses old native shells before their legacy Open
  // implementation can acquire execution. Never fall back to that command.
  return t.core.invoke<CloudProjectOpen | null>("pro_copy_project", { workspaceId, ...accountGuard(expectedAccountLifetime) }).catch(reason => {
    const detail = reason instanceof Error ? reason.message : typeof reason === "string" ? reason : "";
    if (/^Command pro_copy_project not found$/i.test(detail)) throw new Error("project_copy_update_required");
    throw reason;
  });
}

export async function proTakeReturn(): Promise<boolean> {
  return (await tauri()?.core.invoke<boolean>("pro_take_return")) ?? false;
}

export function onProReturn(handler: () => void): Promise<() => void> {
  const t = tauri();
  if (t === null) return Promise.resolve(() => {});
  return t.webviewWindow.getCurrentWebviewWindow().listen<null>("pro-return", () => handler());
}

export interface ProHost {
  alias: string;
  kept: boolean;
  status: "connected" | "connecting" | "prompting" | "offline";
  kind: "ssh" | "device" | "worker";
}

export interface ProDevice {
  id: string;
  installation_id?: string | null;
  name: string;
  last_seen: string;
  this: boolean;
}

export type ProAuthScreenHint = "sign-up" | "sign-in";

export function onProChanged(handler: () => void): Promise<() => void> {
  return tauri()?.event.listen<null>("pro-changed", () => handler()) ?? Promise.resolve(() => {});
}

export type CloudProviderState = "missing" | "needs_sign_in" | "signed_in" | "unknown" | "unavailable";
export interface CloudProviderStatus {
  id: string;
  label: string;
  category: "agent" | "repository";
  installed: boolean | null;
  state: CloudProviderState;
  reason: string | null;
  checked_at: number | null;
  methods: string[];
  /** Older daemons omit this capability; never infer it from sign-in support. */
  disconnect_supported?: boolean;
}
/** A provider row as the last catalog read showed it (the app's or this
 * browser's memory): the catalog's own fields that rendering needs. */
export type RememberedProvider = Pick<CloudProviderStatus, "id" | "label" | "category" | "state"> & Partial<Pick<CloudProviderStatus, "methods" | "disconnect_supported">>;
export interface CloudProviderConnection {
  id: string;
  provider_id: string;
  operation?: "connect" | "disconnect";
  phase: "preparing" | "waiting" | "verifying" | "connected" | "disconnected" | "failed" | "canceled" | "expired";
  expires_at: number;
  action: { type: "device_code"; verification_url: string; user_code: string }
    | { type: "browser"; url: string; input?: "authorization_code" }
    /** A terminal on the cloud: its own agent setup while `preparing`, or an
     * older cloud's GitHub sign-in while `waiting`. Never opened from here
     * (`pro/providers.ts` `olderCloudSignIn`). */
    | { type: "terminal" } | null;
  error_code: string | null;
}
export interface CloudBlockedProvider {
  id: string;
  state: CloudProviderState;
  reason: string | null;
}
export interface CloudPendingHandoff {
  workspace_id: string;
  name: string;
  expected_epoch: number;
  blocked_providers: CloudBlockedProvider[];
}
export interface CloudSetupInfo {
  available?: boolean;
  ssh_public_key?: string | null;
  workspace_id?: string;
  session_id?: string;
  providers?: CloudProviderStatus[];
  connection?: CloudProviderConnection | null;
  handoffs?: CloudPendingHandoff[];
}
export type CloudSetupRequest = { operation: "info" | "start" | "providers" }
  | { operation: "provider_connect"; provider_id: string }
  | { operation: "provider_disconnect"; provider_id: string; acknowledge_cloud_work: true }
  | { operation: "provider_submit"; connection_id: string; code: string }
  | { operation: "provider_connection" | "provider_cancel" | "open_provider_browser"; connection_id: string }
  | { operation: "resume_handoff"; workspace_id: string; expected_epoch: number }
  | { operation: "project"; url: string; name?: string };

export interface LocalProjectCopy { state: "ready" | "pending" | "taking_over" | "recovery_needed"; ready: boolean; checkpoint?: unknown | null; owner_epoch?: number | null }
export type GitStagingStatus = { state: "uncaptured" } | { state: "synced" } | { state: "conflicts"; paths: string[]; total: number; recovery: string };
export interface MirrorWorkspace {
  workspace_id: string; name: string; root: string; never_mirror: boolean; privacy_pending?: boolean;
  checkpoint_id?: string | null;
  local_copy?: LocalProjectCopy | null;
  git_staging?: GitStagingStatus;
  execution_allowed?: boolean;
  ownership: { state: "awaiting_verification" | "local" | "remote" | "transferring" | "hydrating" | "setting_up" | "privacy_disabled"; epoch: number; holder?: string } | null;
  mirror: { files: number; bytes: number; excluded: number; too_large: number; last_mirrored_at: number | null; storage_limit_bytes: number; error: string | null;
    /** Additive (newer daemons): a stable code for `error`, and the files the last return kept in both versions. */
    error_code?: string | null; kept_both?: number; kept_paths?: string[]; git_staging?: GitStagingStatus } | null;
  git_branches?: string[] | null;
  blocked_providers?: CloudBlockedProvider[];
  /** Additive: where this project's work runs now (another computer by its
   *  account name, which may be absent), or null when it is not synced. */
  place?: { where: "here" } | { where: "cloud" } | { where: "computer"; computer?: string | null } | null;
  /** Additive: why the work is not where it would be, in the daemon's closed
   *  plain categories (`agent_not_connected_in_cloud`, `cloud_time_used_up`,
   *  …). Values the UI does not know read as a generic true sentence. */
  reason?: string | null;
  /** Additive: whether "Run here" (`POST /pro/projects/{id}/here`) and "Run
   *  in the cloud" (`POST /pro/projects/{id}/cloud`) apply right now. */
  run_here?: boolean;
  run_in_cloud?: boolean;
}
export interface MirrorStatus {
  configured: boolean; projects_root: string; projects_root_confirmed: boolean; workspaces: MirrorWorkspace[];
  /** Additive: the account connection lapsed and the app is renewing it
   * (then `configured` is false too). */
  renewal_failed?: boolean;
  sessions: { id: string; workspace_id: string; display_name?: string; name: string }[];
}
export async function proMirrorStatus(expectedAccountLifetime?: string): Promise<MirrorStatus> {
  const t = tauri(); if (t === null) throw new Error("Open the desktop app to see your synced projects.");
  return t.core.invoke<MirrorStatus>("pro_mirror_status", { ...accountGuard(expectedAccountLifetime) });
}
