<script lang="ts">
  /**
   * The source-control panel: a singleton pane surface (like Settings).
   * Deliberately simple — branch header, the changed files grouped by staged /
   * changes / untracked / conflicts, and click-to-diff. It reads the same
   * gitStatus store the tree decoration uses, so it is always in sync.
   *
   * Clicking a row opens the diff in an ADJACENT pane (the openFileFrom
   * grammar), so the panel stays visible beside what you are reviewing.
   */
  import type { LayoutCtrl } from "./dnd";
  import {
    gitStatus,
    gitWorktrees,
    refreshGit,
    worktreeForPath,
    type DiffMode,
    type GitEntry,
  } from "./git";
  import { decoFor } from "./gitDeco";
  import { midTruncate } from "./files";
  import type { Session } from "./sessions";
  import FileIcon from "./FileIcon.svelte";
  import SessionGlyph from "./SessionGlyph.svelte";

  interface Props {
    wsId: string | null;
    paneId: string;
    ctrl: LayoutCtrl;
    /** Every live session (daemon-wide): agents in OTHER worktrees of this repo
     *  belong in the Branches view too — that is the whole point of it. */
    sessions: Map<string, Session>;
    names: Map<string, string>;
  }
  let { wsId, paneId, ctrl, sessions, names }: Props = $props();

  const status = $derived($gitStatus);
  const entries = $derived(status?.entries ?? []);

  // An entry can sit in two groups at once (staged edit + further worktree
  // edit) — VS Code semantics; the letter badge tells them apart.
  const conflicts = $derived(entries.filter((e) => e.conflicted));
  const staged = $derived(entries.filter((e) => e.staged && !e.conflicted));
  const changes = $derived(entries.filter((e) => e.unstaged && !e.untracked && !e.conflicted));
  const untracked = $derived(entries.filter((e) => e.untracked));

  const groups = $derived(
    [
      { key: "conflicts", title: "Conflicts", rows: conflicts, mode: "unstaged" as DiffMode },
      { key: "staged", title: "Staged", rows: staged, mode: "staged" as DiffMode },
      { key: "changes", title: "Changes", rows: changes, mode: "unstaged" as DiffMode },
      { key: "untracked", title: "Untracked", rows: untracked, mode: "unstaged" as DiffMode },
    ].filter((g) => g.rows.length > 0),
  );

  const clean = $derived(status !== null && entries.length === 0);

  // Branches: each worktree of the repo, and which sessions live in it. The
  // agent↔branch edge is DERIVED from the session's cwd — nothing is stored.
  // A single-worktree repo shows nothing here: the header already names the
  // branch, and an empty section is chrome that hasn't earned its pixels.
  const worktrees = $derived($gitWorktrees);
  const allBranches = $derived(
    worktrees.length < 2
      ? []
      : worktrees.map((wt) => ({
          wt,
          sessions: [...sessions.values()]
            .filter((s) => s.alive)
            .filter((s) => worktreeForPath(worktrees, s.cwd_current ?? s.cwd)?.path === wt.path),
        })),
  );
  // Only what you can act on: the worktree you're in, and any holding sessions.
  // A repo can carry dozens of stale worktrees (this one does) — listing them
  // all is chrome that hasn't earned its pixels, so the rest fold into a count.
  const branches = $derived(allBranches.filter((b) => b.wt.current || b.sessions.length > 0));
  const otherWorktrees = $derived(allBranches.length - branches.length);

  function openDiff(e: MouseEvent, entry: GitEntry, mode: DiffMode): void {
    ctrl.openDiffFrom(paneId, entry.path, mode, e.metaKey || e.ctrlKey);
  }
</script>

<div class="git-view">
  <header class="ghead">
    {#if status === null}
      <span class="branch none">{wsId === null ? "no workspace" : "not a git repository"}</span>
    {:else}
      <svg class="bicon" viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
        <path
          d="M5 3v7.5M5 12.5v.5M11 3v3a2.5 2.5 0 0 1-2.5 2.5H5"
          fill="none"
          stroke="currentColor"
          stroke-width="1.3"
          stroke-linecap="round"
        />
        <circle cx="5" cy="12.6" r="1.6" fill="none" stroke="currentColor" stroke-width="1.3" />
        <circle cx="5" cy="2.4" r="1.6" fill="none" stroke="currentColor" stroke-width="1.3" />
        <circle cx="11" cy="2.4" r="1.6" fill="none" stroke="currentColor" stroke-width="1.3" />
      </svg>
      <span class="branch" title={status.upstream ?? "no upstream"}>
        {#if status.detached}
          <span class="detached">detached</span>
          <span class="sha">{status.head ?? "?"}</span>
        {:else}
          {status.branch ?? "(unborn)"}
        {/if}
      </span>
      {#if status.ahead > 0}<span class="ab" title="commits ahead of upstream">↑{status.ahead}</span
        >{/if}
      {#if status.behind > 0}<span class="ab" title="commits behind upstream">↓{status.behind}</span
        >{/if}
      <span class="spacer"></span>
      {#if status.error}
        <span class="gerr" title={status.error}>status unavailable</span>
      {/if}
      <button class="refresh" title="refresh git status" onclick={() => refreshGit()}>
        <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
          <path
            d="M13 8a5 5 0 1 1-1.6-3.7M13 2.5V5.5H10"
            fill="none"
            stroke="currentColor"
            stroke-width="1.3"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
        </svg>
      </button>
    {/if}
  </header>

  <div class="glist">
    {#if status === null}
      <div class="empty">
        {wsId === null
          ? "Open a workspace to see its git state."
          : "This workspace is not inside a git repository."}
      </div>
    {:else if clean}
      <div class="empty">Working tree clean.</div>
    {:else}
      {#each groups as g (g.key)}
        <div class="group">
          <div class="gtitle">
            <span>{g.title}</span>
            <span class="gcount">{g.rows.length}</span>
          </div>
          {#each g.rows as entry (entry.path + g.key)}
            {@const deco = decoFor(entry)}
            <button
              class="grow"
              title={entry.path}
              onclick={(e) => openDiff(e, entry, g.mode)}
            >
              <span class="gicon"><FileIcon path={entry.path} size={13} /></span>
              <span class="gname">{midTruncate(entry.rel, 58)}</span>
              {#if entry.orig_rel}
                <span class="gfrom" title={`renamed from ${entry.orig_rel}`}>←</span>
              {/if}
              <span class="gbadge" style:color={deco.color} title={deco.label}>{deco.letter}</span>
            </button>
          {/each}
        </div>
      {/each}
      {#if status.truncated}
        <div class="trunc">Too many changes to list them all.</div>
      {/if}
    {/if}

    {#if branches.length > 0}
      <div class="group branches">
        <div class="gtitle">
          <span>Branches</span>
          <span class="gcount">{branches.length}</span>
        </div>
        {#each branches as b (b.wt.path)}
          <div class="wt" class:current={b.wt.current}>
            <div class="wt-head" title={b.wt.path}>
              <span class="wt-branch">
                {#if b.wt.detached}
                  <span class="detached">detached</span> <span class="sha">{b.wt.head ?? "?"}</span>
                {:else}
                  {b.wt.branch ?? "(unborn)"}
                {/if}
              </span>
              {#if b.wt.current}<span class="wt-tag">current</span>{/if}
              {#if b.wt.locked}<span class="wt-tag muted">locked</span>{/if}
              {#if b.wt.prunable}<span class="wt-tag muted">prunable</span>{/if}
              {#if b.sessions.length > 0}
                <span class="wt-count">{b.sessions.length}</span>
              {/if}
            </div>
            {#each b.sessions as s (s.id)}
              <div class="wt-session" title={s.cwd_current ?? s.cwd}>
                <SessionGlyph kind={s.kind} agentKind={s.agent_kind} size={10} title={s.kind} />
                <span class="wt-session-name">{names.get(s.id) ?? s.name}</span>
              </div>
            {/each}
          </div>
        {/each}
        {#if otherWorktrees > 0}
          <div class="wt-more">
            {otherWorktrees} other worktree{otherWorktrees === 1 ? "" : "s"}, no sessions
          </div>
        {/if}
      </div>
    {/if}
  </div>
</div>

<style>
  .git-view {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    background: var(--term-bg);
  }

  .ghead {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.4rem;
    height: 30px;
    padding: 0 0.6rem;
    border-bottom: 1px solid var(--edge);
    font-size: 0.72rem;
    color: var(--muted);
  }

  .bicon {
    flex: none;
    color: var(--muted);
  }

  .branch {
    font-family: var(--mono);
    color: var(--fg);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .branch.none {
    color: var(--muted);
    font-family: inherit;
  }
  .detached {
    color: var(--warn);
  }
  .sha {
    opacity: 0.8;
  }

  .ab {
    flex: none;
    font-family: var(--mono);
    font-size: 0.68rem;
    font-variant-numeric: tabular-nums;
    color: var(--muted);
  }

  .gerr {
    color: var(--warn);
    font-size: 0.68rem;
  }

  .spacer {
    flex: 1;
  }

  .refresh {
    flex: none;
    appearance: none;
    border: none;
    background: none;
    color: var(--muted);
    cursor: pointer;
    display: flex;
    align-items: center;
    padding: 0.2rem;
    border-radius: 4px;
    transition:
      background-color 0.1s ease,
      color 0.1s ease;
  }
  .refresh:hover {
    background: var(--row-hover);
    color: var(--fg);
  }

  .glist {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 0.35rem 0 0.6rem;
  }

  .group {
    margin-bottom: 0.35rem;
  }

  .gtitle {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    padding: 0.3rem 0.7rem 0.2rem;
    font-size: 0.62rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--muted);
  }

  .gcount {
    font-variant-numeric: tabular-nums;
    opacity: 0.75;
  }

  .grow {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    width: 100%;
    height: 22px;
    padding: 0 0.7rem;
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    text-align: left;
    cursor: pointer;
    color: var(--muted);
  }
  .grow:hover {
    background: var(--row-hover);
  }
  .grow:focus-visible {
    outline: 1px solid var(--focus-ring);
    outline-offset: -1px;
  }

  .gicon {
    flex: none;
    display: flex;
    align-items: center;
  }

  .gname {
    font-family: var(--mono);
    font-size: 0.72rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }
  .grow:hover .gname {
    color: var(--fg);
  }

  .gfrom {
    flex: none;
    opacity: 0.6;
    font-size: 0.7rem;
  }

  .gbadge {
    flex: none;
    margin-left: auto;
    font-family: var(--mono);
    font-size: 0.66rem;
    font-weight: 600;
    line-height: 1;
  }

  /* Branches: one block per worktree, with the sessions living in it. */
  .branches {
    margin-top: 0.35rem;
    border-top: 1px solid var(--edge);
    padding-top: 0.2rem;
  }

  .wt {
    padding: 0.1rem 0.7rem 0.25rem;
  }

  .wt-head {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    height: 20px;
  }

  .wt-branch {
    font-family: var(--mono);
    font-size: 0.72rem;
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .wt.current .wt-branch {
    color: var(--fg);
  }

  .wt-tag {
    flex: none;
    font-size: 0.58rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    padding: 0.03rem 0.28rem;
    border-radius: 3px;
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 14%, transparent);
  }
  .wt-tag.muted {
    color: var(--muted);
    background: var(--row-hover);
  }

  .wt-count {
    flex: none;
    margin-left: auto;
    font-family: var(--mono);
    font-size: 0.64rem;
    font-variant-numeric: tabular-nums;
    color: var(--muted);
  }

  .wt-session {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    padding: 0 0 0 0.9rem;
    height: 18px;
    color: var(--muted);
  }

  .wt-session-name {
    font-size: 0.7rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .wt-more {
    padding: 0.15rem 0.7rem 0.2rem;
    font-size: 0.66rem;
    color: var(--muted);
    opacity: 0.7;
  }

  .empty,
  .trunc {
    padding: 1rem 0.8rem;
    font-size: 0.72rem;
    color: var(--muted);
    text-align: center;
  }
  .trunc {
    padding: 0.5rem 0.8rem;
    opacity: 0.8;
  }
</style>
