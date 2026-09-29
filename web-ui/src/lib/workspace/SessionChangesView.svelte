<script lang="ts">
  import {
    fetchGitStatus,
    gitEnv,
    gitStatus,
    type DiffMode,
    type GitEntry,
    type GitStatus,
  } from "./git";
  import { decoFor } from "./gitDeco";
  import { displayName, type Session } from "./sessions";
  import type { LayoutCtrl } from "../layout/dnd";
  import SessionEdits from "./SessionEdits.svelte";

  /**
   * What this session changed, one view with or without git: the files THIS
   * agent touched (its files_touched list plus what its edit record names),
   * each with its edit count (`SessionEdits`). A file with an uncommitted git
   * change opens the same side-by-side diff the Source Control panel uses
   * (ctrl.openDiffFrom — main's git service, the resolved git.path); any
   * other file opens the agent's own edits for it, in order.
   */
  interface Props {
    session: Session;
    wsRoot: string | null;
    paneId: string;
    ctrl: LayoutCtrl;
  }

  let { session, wsRoot, paneId, ctrl }: Props = $props();

  /** The active workspace's mirrored status (the git store follows the ACTIVE
   *  workspace only). */
  const activeStatus = $derived($gitStatus);

  /**
   * This session may live in a LINKED worktree — its own repo/workspace, not
   * the active one the git store mirrors. Cross-referencing its touched files
   * against the active status would mark every row "no current git change", so
   * when the workspace ids differ we fetch the session's OWN workspace status
   * locally; when they match we just reuse the store (no extra fetch).
   */
  let ownStatus = $state<GitStatus | null>(null);
  $effect(() => {
    const wsId = session.workspace_id;
    // Re-fetch as the touched-files list grows so newly-written files decorate.
    void session.files_touched?.length;
    if (activeStatus !== null && activeStatus.workspace_id === wsId) {
      ownStatus = null; // the store already mirrors this workspace
      return;
    }
    let cancelled = false;
    void fetchGitStatus(wsId).then(
      (s) => {
        if (!cancelled) ownStatus = s.repo ? s : null;
      },
      () => {
        if (!cancelled) ownStatus = null;
      },
    );
    return () => {
      cancelled = true;
    };
  });

  /** The status to decorate against: the active store when it already mirrors
   *  this session's workspace, else the session's own fetched status. */
  const status = $derived(
    activeStatus !== null && activeStatus.workspace_id === session.workspace_id
      ? activeStatus
      : ownStatus,
  );
  /** Absolute path -> its git entry, for the touched files. */
  const byPath = $derived.by(() => {
    const m = new Map<string, GitEntry>();
    for (const e of status?.entries ?? []) m.set(e.path, e);
    return m;
  });
  /** Touched files, newest first (files_touched is oldest-first on the wire). */
  const files = $derived([...(session.files_touched ?? [])].reverse());
  const base = $derived(wsRoot ?? session.cwd_current ?? session.cwd);

  /** The comparison to open for an entry — mirrors the Source Control panel:
   *  a purely-staged change diffs staged, everything else diffs the worktree. */
  function modeFor(e: GitEntry): DiffMode {
    return e.staged && !e.unstaged && !e.untracked && !e.conflicted ? "staged" : "unstaged";
  }
</script>

<div class="changes">
  <header class="head">
    <span class="title">Changes · {displayName(session)}</span>
    <span class="count">{files.length} file{files.length === 1 ? "" : "s"}</span>
  </header>

  {#if $gitEnv !== null && !$gitEnv.ok}
    <!-- gitEnv is tracked independently of `repo` (git.ts), so this explains a
         too-old / missing git even where gitStatus is null — unlike the old
         status.git_ok, which is null in exactly that case. -->
    <div class="note">
      Source control needs <b>git ≥ {$gitEnv.min}</b>. Set a newer
      <code>git.path</code> in Settings to see diffs here.
    </div>
  {/if}

  <!-- Commits come first when there are any ("Committed 2", rows like
       History's) — wired in from GET /api/v1/sessions/{id}/git. -->

  <!-- The files, each with its edit count: a click opens the git diff when
       the file has an uncommitted change, else the agent's own edits for it,
       in order — one list, with or without git. -->
  <div class="list">
    <SessionEdits
      sessionId={session.id}
      wsId={session.workspace_id}
      wsRoot={base}
      paths={files}
      repo={status !== null}
      gitMark={(p) => {
        const entry = byPath.get(p);
        return entry !== undefined ? decoFor(entry) : null;
      }}
      onOpenDiff={(p, e) => {
        const entry = byPath.get(p);
        if (entry === undefined) return false;
        ctrl.openDiffFrom(paneId, p, modeFor(entry), e.metaKey || e.ctrlKey);
        return true;
      }}
      onOpenFile={(p, e) => ctrl.openFileFrom(paneId, p, e.metaKey || e.ctrlKey)}
      refreshKey={session.files_touched?.length ?? 0}
    />
  </div>
</div>

<style>
  .changes {
    height: 100%;
    display: flex;
    flex-direction: column;
    min-height: 0;
    overflow-y: auto;
    scrollbar-width: thin;
    background: var(--bg);
    color: var(--fg);
  }
  .head {
    flex: none;
    display: flex;
    align-items: baseline;
    gap: 10px;
    padding: 8px 14px;
    border-bottom: 1px solid var(--edge);
    position: sticky;
    top: 0;
    z-index: 1;
    background: var(--bg);
  }
  .title {
    font-weight: 600;
    font-size: var(--text-md);
  }
  .count {
    color: var(--muted);
    font-size: var(--text-sm);
  }
  .note {
    margin: 10px 12px 0;
    padding: 8px 10px;
    border: 1px solid color-mix(in srgb, var(--warn) 45%, var(--edge));
    border-radius: 6px;
    background: color-mix(in srgb, var(--warn) 8%, transparent);
    color: var(--fg);
    font-size: var(--text-sm);
  }
  .note code {
    font-family: var(--mono, monospace);
    font-size: 0.92em;
  }
  .list {
    padding: 6px 8px;
  }
</style>
