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
import { writable, derived, get, type Readable } from "svelte/store";
import type { DiffTab, GitDetailTab } from "../layout/layout";

import { api, ApiError } from "../net/api";

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
  /** The path is a submodule (absent on old daemons). */
  submodule?: boolean;
}

export interface GitCounts {
  staged: number;
  unstaged: number;
  untracked: number;
  conflicted: number;
  total: number;
}

/** The git binary the daemon resolved, and whether it can drive the service. */
export interface GitEnv {
  /** The resolved git clears the minimum version — the service can run. */
  ok: boolean;
  /** Absolute path (or bare "git") the daemon is invoking. */
  path: string;
  /** How it was found: an explicit setting, the login shell, or PATH. */
  source: "setting" | "login-shell" | "path";
  /** Parsed "MAJOR.MINOR.PATCH", or null when git could not be run at all. */
  version: string | null;
  /** Raw `git --version` line, for the diagnostic. */
  raw: string | null;
  /** The minimum version chimaera needs ("2.15"). */
  min: string;
}

export interface GitStatus {
  repo: boolean;
  workspace_id: string;
  epoch: number;
  /** The checkout this status is of (absent on old daemons). */
  toplevel?: string;
  /** This repository's own epoch (see `onGitNudge`). */
  repo_epoch?: number;
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
  /** False when the resolved git is missing or too old (see `git`). */
  git_ok?: boolean;
  /** The resolved git binary + its version diagnostic. */
  git?: GitEnv;
  /**
   * Present (with `repo:false`, `git_ok:true`) when git resolved fine but
   * couldn't READ this repository — dubious ownership on shared storage, a
   * permission problem, or a timeout on a wedged filesystem. Distinct from a
   * genuine non-repo (where it is absent): the panel turns it into an
   * actionable message instead of a blank "not a git repository".
   */
  repo_error?: string;
}

/** The comparisons a diff can show: the status ones (unstaged / staged /
 *  head), the working tree against a revision ("rev"), or one commit against
 *  its parent ("commit"). */
export type DiffMode = "unstaged" | "staged" | "head" | "rev" | "commit";

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

/** One worktree of the repo (the main checkout, or a linked one). */
export interface GitWorktree {
  /** Absolute working-tree root. */
  path: string;
  /** Short branch name; `null` when detached. */
  branch: string | null;
  head: string | null;
  detached: boolean;
  bare: boolean;
  locked: boolean;
  prunable: boolean;
  /** The worktree the active workspace has checked out. */
  current: boolean;
  /** Created by chimaera under its managed root — the only ones it removes. */
  managed: boolean;
  /** A managed worktree whose HEAD the main checkout's branch already
   *  contains (null = not asked: unmanaged, or the current one). */
  merged?: boolean | null;
  /** Commits it has that the main checkout's branch lacks (null for the main
   *  checkout itself). */
  ahead_of_main?: number | null;
  behind_main?: number | null;
}

/** One local branch (read-only: there is no checkout). */
export interface GitBranch {
  name: string;
  /** Committer time of its tip, seconds since the epoch. */
  time: number;
  upstream: string | null;
  ahead: number;
  behind: number;
  /** Its upstream was deleted. */
  gone: boolean;
  /** Checked out in the workspace's own checkout. */
  current: boolean;
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

/** One of a workspace's repositories (the daemon's `GET /git/repos`). */
export interface GitRepo {
  /** Its top level. */
  path: string;
  /** Path in the workspace ("." for the root); null for a repository that
   *  encloses the root. */
  rel: string | null;
  kind: "root" | "enclosing" | "nested" | "submodule";
  submodule: boolean;
  /** The repository it sits in, if any. */
  parent: string | null;
  branch: string | null;
  detached: boolean;
  head: string | null;
  epoch: number;
}

export interface GitRepoList {
  workspace_id: string;
  git_ok: boolean;
  git?: GitEnv;
  repos: GitRepo[];
  /** More repositories exist than the daemon lists (32). */
  capped: boolean;
  /** Why git couldn't read the repository at the root, if it couldn't. */
  primary_error?: string | null;
  epoch: number;
}

function gitQuery(workspaceId: string, extra: Record<string, string | undefined> = {}): string {
  const q = new URLSearchParams({ workspace_id: workspaceId });
  for (const [k, v] of Object.entries(extra)) if (v !== undefined && v !== "") q.set(k, v);
  return q.toString();
}

export async function fetchGitRepos(workspaceId: string, refresh = false): Promise<GitRepoList> {
  return json(
    await api(`/git/repos?${gitQuery(workspaceId, { refresh: refresh ? "true" : undefined })}`),
  );
}

export async function fetchGitStatus(workspaceId: string, repo?: string): Promise<GitStatus> {
  return json(await api(`/git/status?${gitQuery(workspaceId, { repo })}`));
}

export async function fetchGitDiff(
  workspaceId: string,
  path: string,
  mode: DiffMode,
  opts: { repo?: string; rev?: string; orig?: string } = {},
): Promise<GitDiff> {
  return json(
    await api(
      `/git/diff?${gitQuery(workspaceId, {
        path,
        mode: mode === "rev" ? undefined : mode,
        repo: opts.repo,
        rev: opts.rev,
        orig: opts.orig,
      })}`,
    ),
  );
}

/** Where a session's repository stood (`GET /sessions/{id}/git`). */
export interface SessionGitAnchor {
  repo: string;
  worktree: string;
  branch: string | null;
  head: string | null;
  at_ms: number;
}

/** A session's git story: where it started, where it stands (or ended), and
 *  the commits in between (newest first, ≤50). */
export interface SessionGitStory {
  session_id: string;
  live: boolean;
  start: SessionGitAnchor | null;
  current: SessionGitAnchor | null;
  commits: { sha: string; subject: string; time: number }[];
  truncated: boolean;
  rewritten: boolean;
  branch_changed: boolean;
  repo_changed: boolean;
}

export async function fetchSessionGit(sessionId: string): Promise<SessionGitStory> {
  return json(await api(`/sessions/${encodeURIComponent(sessionId)}/git`));
}

/** One commit of a history page. */
export interface GitCommit {
  sha: string;
  parents: string[];
  author: string;
  /** Author time, seconds since the epoch. */
  time: number;
  subject: string;
  /** The message after the subject (capped), for the row's hover. */
  body?: string;
}

export interface GitLogPage {
  toplevel: string;
  path?: string | null;
  commits: GitCommit[];
  has_more: boolean;
  /** The branch has no commits yet. */
  unborn?: boolean;
}

/** One page of history (≤50): a repository's, one file's (renames
 *  followed), or from a branch/revision. */
export async function fetchGitLog(
  workspaceId: string,
  opts: { repo?: string; path?: string; rev?: string; skip?: number; limit?: number } = {},
): Promise<GitLogPage> {
  return json(
    await api(
      `/git/log?${gitQuery(workspaceId, {
        repo: opts.repo,
        path: opts.path,
        rev: opts.rev,
        skip: opts.skip ? String(opts.skip) : undefined,
        limit: opts.limit ? String(opts.limit) : undefined,
      })}`,
    ),
  );
}

/** One file a commit (or a branch) changed. */
export interface GitChangedFile {
  path: string;
  rel: string;
  orig: string | null;
  orig_rel: string | null;
  /** A M D R C T, or "?" for untracked work on a branch. */
  status: string;
  added: number | null;
  removed: number | null;
  binary: boolean;
}

export interface GitCommitDetail {
  toplevel: string;
  sha: string;
  parents: string[];
  author: string;
  time: number;
  committer: string;
  commit_time: number;
  subject: string;
  body: string;
  files: GitChangedFile[];
  truncated: boolean;
}

export async function fetchGitShow(
  workspaceId: string,
  rev: string,
  repo?: string,
): Promise<GitCommitDetail> {
  return json(await api(`/git/show?${gitQuery(workspaceId, { rev, repo })}`));
}

/** "Changes on this branch": everything since it left its base. */
export interface GitBranchChanges {
  toplevel: string;
  /** What it is compared against (a branch name), or null when there is
   *  none and only uncommitted work shows. */
  base: string | null;
  merge_base: string | null;
  /** The revision each file's diff opens against. */
  diff_from: string;
  head: string | null;
  /** Commits on the branch since it left the base. */
  ahead: number | null;
  files: GitChangedFile[];
  truncated: boolean;
}

export async function fetchGitCompare(
  workspaceId: string,
  repo?: string,
  base?: string,
): Promise<GitBranchChanges> {
  return json(await api(`/git/compare?${gitQuery(workspaceId, { repo, base })}`));
}

export async function fetchGitWorktrees(
  workspaceId: string,
  repo?: string,
): Promise<{ repo: boolean; worktrees: GitWorktree[] }> {
  return json(await api(`/git/worktrees?${gitQuery(workspaceId, { repo })}`));
}

export async function fetchGitBranches(
  workspaceId: string,
  repo?: string,
): Promise<{ repo: boolean; branches: GitBranch[]; truncated?: boolean }> {
  return json(await api(`/git/branches?${gitQuery(workspaceId, { repo })}`));
}

export interface CreatedWorktree {
  worktree: { path: string; branch: string };
  /** The worktree is registered as a workspace, so the branch is openable. */
  workspace: { id: string; root: string; name: string };
  /** What `.worktreeinclude` copied over (ignored files such as `.env`). */
  included?: { copied: number; bytes: number; capped: boolean; names?: string[] };
}

/**
 * Create a worktree for `branch` under the daemon's managed root and register it
 * as a workspace. Additive — it never touches an existing checkout. The daemon
 * rejects names git would refuse, and 409s if that branch is already checked out.
 * `repo` picks one of the workspace's repositories (the root's when omitted).
 */
export async function createWorktree(
  workspaceId: string,
  branch: string,
  base?: string,
  repo?: string,
): Promise<CreatedWorktree> {
  return json(
    await api(`/git/worktrees`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        workspace_id: workspaceId,
        branch,
        ...(base ? { base } : {}),
        ...(repo ? { repo } : {}),
      }),
    }),
  );
}

/**
 * Remove a MANAGED worktree. Destructive: the daemon refuses anything it did not
 * create, the worktree this workspace is open on, one holding a live session, or
 * one with uncommitted or unshared work (unless `force`). The branch survives.
 */
export async function removeWorktree(
  workspaceId: string,
  path: string,
  force = false,
  repo?: string,
): Promise<void> {
  const res = await api(`/git/worktrees`, {
    method: "DELETE",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ workspace_id: workspaceId, path, force, ...(repo ? { repo } : {}) }),
  });
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
}

/**
 * Bumped whenever the daemon's workspace registry changed underneath the UI
 * (a worktree was created or removed). App re-fetches its workspace list.
 */
export const workspacesChanged = writable(0);
export function notifyWorkspacesChanged(): void {
  workspacesChanged.update((n) => n + 1);
}

// ---- reactive store (active workspace only) ---------------------------------

const statusStore = writable<GitStatus | null>(null);
/** The active workspace's primary repository status — the one at or around
 *  its root (`null` = none / not loaded yet). */
export const gitStatus: Readable<GitStatus | null> = statusStore;

const worktreesStore = writable<GitWorktree[]>([]);
/** Every worktree of the active workspace's primary repository. */
export const gitWorktrees: Readable<GitWorktree[]> = worktreesStore;

const reposStore = writable<GitRepo[]>([]);
/** The active workspace's repositories: its primary (when there is one)
 *  first, then those below its root. One entry = today's single-repo view. */
export const gitRepos: Readable<GitRepo[]> = reposStore;

const reposCappedStore = writable(false);
/** The daemon lists at most 32 repositories; true when there are more. */
export const gitReposCapped: Readable<boolean> = reposCappedStore;

const repoStatusesStore = writable<Map<string, GitStatus>>(new Map());
/** Statuses of the repositories BELOW the root, by top level (the primary's
 *  is `gitStatus`). */
export const gitRepoStatuses: Readable<Map<string, GitStatus>> = repoStatusesStore;

const gitEnvStore = writable<GitEnv | null>(null);
/**
 * The daemon's resolved git binary + version. Tracked independently of `repo`
 * so the panel can explain a too-old / missing git (`ok:false`) even where
 * there is no repo to show — the "(unborn)"-looking dead end on old HPC git.
 */
export const gitEnv: Readable<GitEnv | null> = gitEnvStore;

const gitRepoErrorStore = writable<string | null>(null);
/**
 * Why a resolved-fine git couldn't read the workspace's repo (dubious
 * ownership, permission, timeout), or `null`. Tracked alongside `gitEnv` so the
 * panel can explain it even though `gitStatus` is null (no repo to render).
 */
export const gitRepoError: Readable<string | null> = gitRepoErrorStore;

const expandedReposStore = writable<Set<string>>(new Set());
/** Repository sections open in the Source Control panel (by top level):
 *  with the repositories holding mounted files, what the daemon's backstop
 *  watches. Owned here so it survives the panel remounting. */
export const gitExpandedRepos: Readable<Set<string>> = expandedReposStore;
export function setRepoExpanded(path: string, open: boolean): void {
  expandedReposStore.update((set) => {
    const next = new Set(set);
    if (open) next.add(path);
    else next.delete(path);
    return next;
  });
}

const focusStore = writable<string | null>(null);
/** The focused file's or session's folder (App keeps it current): the panel
 *  auto-expands the repository holding it, and the status chip follows it. */
export const gitFocus: Readable<string | null> = focusStore;
export function setGitFocus(path: string | null): void {
  focusStore.set(path);
}

const revealStore = writable<{ path: string; n: number } | null>(null);
/** A request to scroll the panel to one repository (the status chip's click). */
export const gitRevealRepo: Readable<{ path: string; n: number } | null> = revealStore;
let revealN = 0;
export function revealRepo(path: string): void {
  expandedReposStore.update((set) => new Set(set).add(path));
  revealStore.set({ path, n: ++revealN });
}

/** A Source Control section's open state, remembered per workspace in this
 *  browser (a per-viewer convenience: storage may be unavailable). */
export function gitSectionOpen(wsId: string | null, key: string, fallback: boolean): boolean {
  try {
    const v = localStorage.getItem(`chimaera.git.section.${wsId ?? "-"}.${key}`);
    return v === null ? fallback : v === "1";
  } catch {
    return fallback;
  }
}
export function rememberGitSection(wsId: string | null, key: string, open: boolean): void {
  try {
    localStorage.setItem(`chimaera.git.section.${wsId ?? "-"}.${key}`, open ? "1" : "0");
  } catch {
    // storage blocked: the section just won't be remembered
  }
}

/** "2 commits to push · 1 to pull from origin/main" — the ↑↓ in words. */
export function aheadBehindWords(ahead: number, behind: number, upstream: string | null): string {
  const parts: string[] = [];
  if (ahead > 0) parts.push(`${ahead} commit${ahead === 1 ? "" : "s"} to push`);
  if (behind > 0) parts.push(`${behind} to pull${upstream ? ` from ${upstream}` : ""}`);
  return parts.join(" · ");
}

let currentWs: string | null = null;
let refreshSeq = 0;
const lastEpoch = new Map<string, number>();
/** Last applied epoch per repository top level (active workspace). */
let lastRepoEpochs = new Map<string, number>();
let primaryTop: string | null = null;

/** Point the store at a workspace (or `null`) and fetch its status. */
export async function activateGitWorkspace(wsId: string | null): Promise<void> {
  if (wsId === currentWs) return;
  currentWs = wsId;
  statusStore.set(null);
  worktreesStore.set([]);
  reposStore.set([]);
  reposCappedStore.set(false);
  repoStatusesStore.set(new Map());
  expandedReposStore.set(new Set());
  gitRepoErrorStore.set(null);
  lastRepoEpochs = new Map();
  primaryTop = null;
  if (wsId) await refresh(wsId);
}

async function refresh(wsId: string, probe = false): Promise<void> {
  const seq = ++refreshSeq;
  try {
    // `git worktree list` is cheap (reads refs), and a checkout in ANOTHER
    // worktree only surfaces here, so it rides every refresh. The repository
    // list is a cached daemon answer (the probe runs once per open).
    const [status, wt, repos] = await Promise.all([
      fetchGitStatus(wsId),
      fetchGitWorktrees(wsId).catch(() => ({ repo: false, worktrees: [] })),
      fetchGitRepos(wsId, probe).catch(() => null),
    ]);
    // Drop stale responses (workspace switched or a newer refresh overtook us).
    if (currentWs !== wsId || seq !== refreshSeq) return;
    if (typeof status.epoch === "number") lastEpoch.set(wsId, status.epoch);
    // Git-binary diagnostic rides every status response, repo or not — so a
    // too-old git surfaces even when there's no repo to render.
    gitEnvStore.set(status.git ?? null);
    // "not a repo" leaves this null; a real repo we couldn't read carries the
    // reason so the panel can explain it (see repo_error).
    gitRepoErrorStore.set(status.repo ? null : (status.repo_error ?? null));
    statusStore.set(status.repo ? status : null);
    worktreesStore.set(status.repo ? (wt.worktrees ?? []) : []);
    primaryTop = status.repo ? (status.toplevel ?? null) : null;
    if (primaryTop !== null && typeof status.repo_epoch === "number") {
      lastRepoEpochs.set(primaryTop, status.repo_epoch);
    }
    if (repos !== null) applyRepos(wsId, repos);
  } catch {
    if (currentWs === wsId && seq === refreshSeq) {
      statusStore.set(null);
      worktreesStore.set([]);
    }
  }
}

/** Take a fresh repository list; fetch the statuses of the ones below the
 *  root that are new (or whose epoch moved since we last applied it). */
function applyRepos(wsId: string, list: GitRepoList): void {
  reposStore.set(list.repos ?? []);
  reposCappedStore.set(list.capped === true);
  const below = (list.repos ?? []).filter((r) => r.kind === "nested" || r.kind === "submodule");
  const keep = new Set(below.map((r) => r.path));
  repoStatusesStore.update((m) => {
    const next = new Map([...m].filter(([k]) => keep.has(k)));
    return next.size === m.size ? m : next;
  });
  for (const r of below) {
    const have = lastRepoEpochs.get(r.path);
    if (have === undefined || have !== r.epoch) void refreshRepo(wsId, r.path);
  }
}

const repoSeq = new Map<string, number>();

/** Fetch one repository's status (below the root). */
async function refreshRepo(wsId: string, top: string): Promise<void> {
  const seq = (repoSeq.get(top) ?? 0) + 1;
  repoSeq.set(top, seq);
  try {
    const status = await fetchGitStatus(wsId, top);
    if (currentWs !== wsId || repoSeq.get(top) !== seq) return;
    if (typeof status.repo_epoch === "number") lastRepoEpochs.set(top, status.repo_epoch);
    repoStatusesStore.update((m) => new Map(m).set(top, status));
  } catch {
    // A repository that went away: the next list refresh drops it.
  }
}

/**
 * Handle a `{type:"git"}` epoch frame: refetch what moved — this is the whole
 * point of invalidate-and-pull. The workspace epoch moves with every change;
 * `repos` says which repository each change was in, so a change inside a
 * nested repository refetches that one only, and everything else (the
 * primary, worktree changes, a new repository found) refreshes as before.
 */
export function onGitNudge(
  epochs: Record<string, number>,
  repos?: Record<string, Record<string, number>>,
): void {
  if (!currentWs) return;
  const ws = currentWs;
  const epoch = epochs[ws];
  if (typeof epoch !== "number") return;
  const before = lastEpoch.get(ws);
  const moved = repos?.[ws] ?? {};
  let nestedDelta = 0;
  const nestedMoved: string[] = [];
  for (const [top, e] of Object.entries(moved)) {
    if (top === primaryTop) continue;
    const have = lastRepoEpochs.get(top);
    if (have === undefined) {
      // A repository we have no status for yet moved: fetch it (the list
      // refresh below also covers one we don't know at all).
      nestedMoved.push(top);
      continue;
    }
    if (e !== have) {
      nestedDelta += e - have;
      nestedMoved.push(top);
    }
  }
  if (epoch === before && nestedMoved.length === 0) return;
  const primaryMoved = before === undefined || epoch - before > nestedDelta;
  if (primaryMoved) {
    void refresh(ws);
  } else {
    lastEpoch.set(ws, epoch);
  }
  if (!primaryMoved) {
    for (const top of nestedMoved) {
      if (lastRepoEpochs.has(top)) void refreshRepo(ws, top);
    }
  }
}

/** Force a refresh of the active workspace (manual refresh control): the
 *  daemon probes for repositories again, and every repository refetches. */
export function refreshGit(): void {
  if (!currentWs) return;
  const ws = currentWs;
  lastRepoEpochs = new Map();
  void refresh(ws, true);
}

// ---- per-path index + folder rollup (for the tree) --------------------------

/** Coarse category used for the folder rollup dot on collapsed directories. */
export type GitDirCat = "conflicted" | "modified" | "untracked";

export interface GitIndex {
  /** Absolute path -> its status entry (from the innermost repository). */
  files: Map<string, GitEntry>;
  /** Absolute dir path -> the most significant descendant category. */
  dirs: Map<string, GitDirCat>;
  /** Top levels of the repositories below the root (their folders get a
   *  small mark in the tree). */
  repoRoots: Set<string>;
}

function dirRank(c: GitDirCat | undefined): number {
  return c === "conflicted" ? 3 : c === "modified" ? 2 : c === "untracked" ? 1 : 0;
}

function addStatus(
  status: GitStatus,
  nestedTops: Set<string>,
  files: Map<string, GitEntry>,
  dirs: Map<string, GitDirCat>,
): void {
  const top = status.toplevel ?? null;
  for (const entry of status.entries ?? []) {
    const bare = entry.path.endsWith("/") ? entry.path.slice(0, -1) : entry.path;
    // The outer repository sees a nested repository as one untracked folder
    // (or a submodule entry): that folder is a link to its own section, not
    // a change — its files' badges come from the inner status.
    if (nestedTops.has(bare) && (entry.untracked || entry.submodule)) continue;
    files.set(entry.path, entry);
    const cat: GitDirCat = entry.conflicted
      ? "conflicted"
      : entry.untracked
        ? "untracked"
        : "modified";
    // Roll the category up to every ancestor directory so a collapsed folder
    // shows that something inside it changed — stopping at the repository's
    // own folder: a change inside a nested repository never colours the
    // outer repository's folders. Absolute POSIX paths (the daemon is Unix).
    let p = bare;
    for (;;) {
      const slash = p.lastIndexOf("/");
      if (slash <= 0) break;
      if (top !== null && p === top) break;
      p = p.slice(0, slash);
      if (top !== null && p.length < top.length) break;
      if (dirRank(cat) > dirRank(dirs.get(p))) dirs.set(p, cat);
    }
  }
}

function buildIndex(
  primary: GitStatus | null,
  others: Map<string, GitStatus>,
  repos: GitRepo[],
): GitIndex {
  const files = new Map<string, GitEntry>();
  const dirs = new Map<string, GitDirCat>();
  const repoRoots = new Set(
    repos.filter((r) => r.kind === "nested" || r.kind === "submodule").map((r) => r.path),
  );
  // Outer first, inner after: the innermost repository's entry wins a path.
  if (primary?.entries) addStatus(primary, repoRoots, files, dirs);
  const inner = [...others.values()].sort(
    (a, b) => (a.toplevel ?? "").length - (b.toplevel ?? "").length,
  );
  for (const status of inner) addStatus(status, repoRoots, files, dirs);
  return { files, dirs, repoRoots };
}

/** Derived per-path index for the file tree (files + folder rollup). */
export const gitIndex: Readable<GitIndex> = derived(
  [statusStore, repoStatusesStore, reposStore],
  ([$primary, $others, $repos]) => buildIndex($primary, $others, $repos),
);

/** The innermost of `repos` containing `path` (longest top level first). */
export function repoForPath(repos: GitRepo[], path: string | null | undefined): GitRepo | null {
  if (!path) return null;
  let best: GitRepo | null = null;
  for (const r of repos) {
    const root = r.path.endsWith("/") ? r.path : `${r.path}/`;
    if (path === r.path || path.startsWith(root)) {
      if (best === null || r.path.length > best.path.length) best = r;
    }
  }
  return best;
}

// ---- opening git views from anywhere ---------------------------------------

type GitOpener = (tab: DiffTab | GitDetailTab) => void;
let gitOpener: GitOpener | null = null;

/** App-level wiring: how a git view opens from a surface without a pane
 *  controller (the chat's branch line, menus, Quick Open). */
export function setGitOpener(fn: GitOpener | null): void {
  gitOpener = fn;
}

/** Open a git view beside the focused pane. */
export function openGitView(tab: DiffTab | GitDetailTab): void {
  gitOpener?.(tab);
}

/** A file's history (renames followed), from the repository holding it. */
export function openFileHistory(path: string): void {
  const r = repoForPath(get(reposStore), path);
  const repo = r !== null && (r.kind === "nested" || r.kind === "submodule") ? r.path : null;
  openGitView({
    surface: "gitx",
    view: "history",
    repo,
    path,
    title: path.split("/").filter(Boolean).pop() ?? path,
  });
}

/** "Changes on this branch" for the checkout a session works in. */
export function openBranchChanges(git: { worktree: string; branch: string | null }): void {
  openGitView({
    surface: "gitx",
    view: "branch",
    repo: git.worktree,
    title: git.branch ?? (git.worktree.split("/").filter(Boolean).pop() ?? git.worktree),
  });
}

/** The active workspace's history (its primary repository, or `repo`). */
export function openRepoHistory(repo: string | null = null): void {
  openGitView({ surface: "gitx", view: "history", repo });
}
