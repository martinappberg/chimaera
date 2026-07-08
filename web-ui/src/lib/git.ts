/**
 * Client + reactive store for the daemon's read-only git service:
 *   GET /git/status?workspace_id=            porcelain-v2 status (branch + entries)
 *   GET /git/diff?workspace_id=&path=&mode=  before/after blobs for a side-by-side
 *
 * Status is pulled, never pushed: /ws/events carries only a tiny per-workspace
 * epoch nudge (see onGitNudge), so big path lists stay off the firehose. The
 * store mirrors ONLY the active workspace's status; the file tree, pane tabs,
 * and the changes panel all read it. Kept out of the layout tree so it survives
 * tab drags and pane restructuring (same reasoning as editing.ts).
 */
import { writable, derived, type Readable } from "svelte/store";

import { api, ApiError } from "./api";

export interface GitEntry {
  /** Absolute path (matches FsEntry.path, so the tree can look it up directly). */
  path: string;
  /** Repo-relative path. */
  rel: string;
  /** Rename source (absolute), if this is a rename/copy. */
  orig: string | null;
  orig_rel: string | null;
  /** Index (staged) status code; "?" for untracked. */
  x: string;
  /** Worktree (unstaged) status code; "?" for untracked. */
  y: string;
  staged: boolean;
  unstaged: boolean;
  untracked: boolean;
  conflicted: boolean;
}

export interface GitCounts {
  staged: number;
  unstaged: number;
  untracked: number;
  conflicted: number;
  total: number;
}

export interface GitStatus {
  repo: boolean;
  workspace_id: string;
  epoch: number;
  branch: string | null;
  detached: boolean;
  head: string | null;
  upstream: string | null;
  ahead: number;
  behind: number;
  entries: GitEntry[];
  counts: GitCounts;
  truncated: boolean;
  /** Set when the repo exists but status momentarily failed. */
  error?: string;
}

export type DiffMode = "unstaged" | "staged" | "head";

export interface GitDiff {
  path: string;
  rel: string;
  mode: DiffMode;
  binary: boolean;
  too_large?: boolean;
  added?: boolean;
  deleted?: boolean;
  /** Before/after full text (the client's MergeView computes the diff). */
  a: string;
  b: string;
  a_label: string;
  b_label: string;
  error?: string;
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

export async function fetchGitStatus(workspaceId: string): Promise<GitStatus> {
  const q = new URLSearchParams({ workspace_id: workspaceId });
  return json(await api(`/git/status?${q.toString()}`));
}

export async function fetchGitDiff(
  workspaceId: string,
  path: string,
  mode: DiffMode,
): Promise<GitDiff> {
  const q = new URLSearchParams({ workspace_id: workspaceId, path, mode });
  return json(await api(`/git/diff?${q.toString()}`));
}

// ---- reactive store (active workspace only) ---------------------------------

const statusStore = writable<GitStatus | null>(null);
/** The active workspace's git status (`null` = no repo / not loaded yet). */
export const gitStatus: Readable<GitStatus | null> = statusStore;

let currentWs: string | null = null;
let refreshSeq = 0;
const lastEpoch = new Map<string, number>();

/** Point the store at a workspace (or `null`) and fetch its status. */
export async function activateGitWorkspace(wsId: string | null): Promise<void> {
  if (wsId === currentWs) return;
  currentWs = wsId;
  statusStore.set(null);
  if (wsId) await refresh(wsId);
}

async function refresh(wsId: string): Promise<void> {
  const seq = ++refreshSeq;
  try {
    const status = await fetchGitStatus(wsId);
    // Drop stale responses (workspace switched or a newer refresh overtook us).
    if (currentWs !== wsId || seq !== refreshSeq) return;
    if (typeof status.epoch === "number") lastEpoch.set(wsId, status.epoch);
    statusStore.set(status.repo ? status : null);
  } catch {
    if (currentWs === wsId && seq === refreshSeq) statusStore.set(null);
  }
}

/**
 * Handle a `{type:"git"}` epoch frame: refetch iff the active workspace's epoch
 * moved since we last applied it. This is the whole point of invalidate-and-pull.
 */
export function onGitNudge(epochs: Record<string, number>): void {
  if (!currentWs) return;
  const epoch = epochs[currentWs];
  if (typeof epoch !== "number") return;
  if (epoch !== lastEpoch.get(currentWs)) {
    void refresh(currentWs);
  }
}

/** Force a refresh of the active workspace (manual refresh control). */
export function refreshGit(): void {
  if (currentWs) void refresh(currentWs);
}

// ---- per-path index + folder rollup (for the tree) --------------------------

/** Coarse category used for the folder rollup dot on collapsed directories. */
export type GitDirCat = "conflicted" | "modified" | "untracked";

export interface GitIndex {
  /** Absolute path -> its status entry. */
  files: Map<string, GitEntry>;
  /** Absolute dir path -> the most significant descendant category. */
  dirs: Map<string, GitDirCat>;
}

function dirRank(c: GitDirCat | undefined): number {
  return c === "conflicted" ? 3 : c === "modified" ? 2 : c === "untracked" ? 1 : 0;
}

function buildIndex(status: GitStatus | null): GitIndex {
  const files = new Map<string, GitEntry>();
  const dirs = new Map<string, GitDirCat>();
  if (!status?.entries) return { files, dirs };
  for (const entry of status.entries) {
    files.set(entry.path, entry);
    const cat: GitDirCat = entry.conflicted
      ? "conflicted"
      : entry.untracked
        ? "untracked"
        : "modified";
    // Roll the category up to every ancestor directory so a collapsed folder
    // shows that something inside it changed. Absolute POSIX paths (the daemon
    // is Unix), so splitting on "/" is safe.
    let p = entry.path;
    for (;;) {
      const slash = p.lastIndexOf("/");
      if (slash <= 0) break;
      p = p.slice(0, slash);
      if (dirRank(cat) > dirRank(dirs.get(p))) dirs.set(p, cat);
    }
  }
  return { files, dirs };
}

/** Derived per-path index for the file tree (files + folder rollup). */
export const gitIndex: Readable<GitIndex> = derived(statusStore, buildIndex);
