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
    createWorktree,
    gitStatus,
    gitWorktrees,
    notifyWorkspacesChanged,
    refreshGit,
    removeWorktree,
    worktreeForPath,
    type DiffMode,
    type GitEntry,
    type GitWorktree,
  } from "./git";
  import { decoFor } from "./gitDeco";
  import { midTruncate } from "./files";
  import { createSession, type Session, type SessionKind } from "./sessions";
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
    /** Focus a session that may live in another workspace (a worktree branch);
     *  used to reveal a session spawned into a fresh worktree, and the Branches
     *  session rows. */
    onOpenSession: (sessionId: string, workspaceId: string) => void;
  }
  let { wsId, paneId, ctrl, sessions, names, onOpenSession }: Props = $props();

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
  // Only what you can act on: the worktree you're in, any holding sessions, and
  // any Chimaera created (managed — those you can remove here). A repo can carry
  // dozens of the user's own stale worktrees (this one does); listing them all
  // is chrome that hasn't earned its pixels, so the rest fold into a count.
  const branches = $derived(
    allBranches.filter((b) => b.wt.current || b.sessions.length > 0 || b.wt.managed),
  );
  const otherWorktrees = $derived(allBranches.length - branches.length);

  function openDiff(e: MouseEvent, entry: GitEntry, mode: DiffMode): void {
    ctrl.openDiffFrom(paneId, entry.path, mode, e.metaKey || e.ctrlKey);
  }

  // ---- worktree orchestration (the panel's only mutations) ------------------

  // The composer: pick "terminal" or an agent, type a branch, and Chimaera
  // creates the worktree + spawns the session into it. Kept collapsed until the
  // "+ branch" affordance is clicked so the panel stays quiet.
  let composing = $state(false);
  let newBranch = $state("");
  let newKind = $state<SessionKind>("agent");
  let busy = $state(false);
  let actionError = $state<string | null>(null);
  let branchInput = $state<HTMLInputElement | null>(null);

  function startCompose(): void {
    composing = true;
    actionError = null;
    void Promise.resolve().then(() => branchInput?.focus());
  }

  async function spawnInNewBranch(): Promise<void> {
    const branch = newBranch.trim();
    if (busy || wsId === null || branch === "") return;
    busy = true;
    actionError = null;
    try {
      const created = await createWorktree(wsId, branch);
      // The new worktree is its own workspace; spawn the session there and
      // reveal it. `refreshGit` picks up the new branch in the Branches view.
      const session = await createSession(created.workspace.id, newKind);
      notifyWorkspacesChanged();
      refreshGit();
      onOpenSession(session.id, created.workspace.id);
      composing = false;
      newBranch = "";
    } catch (e) {
      actionError = e instanceof Error ? e.message : "failed to create the worktree";
    } finally {
      busy = false;
    }
  }

  async function remove(wt: GitWorktree): Promise<void> {
    if (busy || wsId === null) return;
    // Removal deletes a working tree — a real confirm, with the branch named.
    if (!confirm(`Remove the worktree for "${wt.branch ?? wt.path}"?\n\nThe branch is kept; only this checkout is deleted.`)) {
      return;
    }
    busy = true;
    actionError = null;
    try {
      await removeWorktree(wsId, wt.path);
      notifyWorkspacesChanged();
      refreshGit();
    } catch (e) {
      actionError = e instanceof Error ? e.message : "failed to remove the worktree";
    } finally {
      busy = false;
    }
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

    {#if status !== null && worktrees.length >= 1}
      <div class="group branches">
        <div class="gtitle">
          <span>Branches</span>
          {#if branches.length > 0}<span class="gcount">{branches.length}</span>{/if}
          <span class="spacer"></span>
          <button class="gt-action" title="new branch in its own worktree" onclick={startCompose}>
            + branch
          </button>
        </div>

        {#if composing}
          <!-- Create a worktree for a new branch and spawn a session into it. -->
          <div class="compose">
            <div class="compose-row">
              <div class="seg" role="group" aria-label="session kind">
                <button class="seg-btn" class:on={newKind === "agent"} onclick={() => (newKind = "agent")}
                  >agent</button>
                <button class="seg-btn" class:on={newKind === "shell"} onclick={() => (newKind = "shell")}
                  >terminal</button>
              </div>
              <input
                class="compose-input"
                bind:this={branchInput}
                bind:value={newBranch}
                placeholder="new-branch-name"
                spellcheck="false"
                autocapitalize="off"
                autocorrect="off"
                disabled={busy}
                onkeydown={(e) => {
                  if (e.key === "Enter") void spawnInNewBranch();
                  else if (e.key === "Escape") {
                    composing = false;
                    newBranch = "";
                  }
                }}
              />
            </div>
            <div class="compose-actions">
              <button
                class="compose-go"
                disabled={busy || newBranch.trim() === ""}
                onclick={() => void spawnInNewBranch()}>{busy ? "creating…" : "create + open"}</button>
              <button class="compose-cancel" disabled={busy} onclick={() => (composing = false)}>cancel</button>
            </div>
          </div>
        {/if}
        {#if actionError !== null}
          <div class="wt-error" role="alert">{actionError}</div>
        {/if}

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
              <!-- Remove only where the daemon would allow it: a managed
                   worktree that is neither the current one nor holding sessions. -->
              {#if b.wt.managed && !b.wt.current && b.sessions.length === 0}
                <button
                  class="wt-remove"
                  title="remove this worktree (keeps the branch)"
                  aria-label="remove worktree"
                  disabled={busy}
                  onclick={() => void remove(b.wt)}>&times;</button>
              {/if}
            </div>
            {#each b.sessions as s (s.id)}
              <button
                class="wt-session"
                title={s.cwd_current ?? s.cwd}
                onclick={() => onOpenSession(s.id, s.workspace_id)}
              >
                <SessionGlyph kind={s.kind} agentKind={s.agent_kind} size={10} title={s.kind} />
                <span class="wt-session-name">{names.get(s.id) ?? s.name}</span>
              </button>
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
    width: 100%;
    padding: 0 0.7rem 0 0.9rem;
    height: 18px;
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    text-align: left;
    cursor: pointer;
    color: var(--muted);
  }
  .wt-session:hover {
    background: var(--row-hover);
    color: var(--fg);
  }

  .wt-session-name {
    font-size: 0.7rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .wt-remove {
    flex: none;
    appearance: none;
    border: none;
    background: none;
    color: var(--muted);
    cursor: pointer;
    font-size: 0.9rem;
    line-height: 1;
    padding: 0 0.15rem;
    border-radius: 3px;
    opacity: 0;
    transition:
      opacity 0.1s ease,
      color 0.1s ease,
      background-color 0.1s ease;
  }
  .wt:hover .wt-remove {
    opacity: 0.7;
  }
  .wt-remove:hover {
    opacity: 1;
    color: var(--git-deleted);
    background: var(--row-hover);
  }

  .wt-more {
    padding: 0.15rem 0.7rem 0.2rem;
    font-size: 0.66rem;
    color: var(--muted);
    opacity: 0.7;
  }

  /* The gtitle action ("+ branch") sits at the section header's right. */
  .gt-action {
    flex: none;
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: 0.64rem;
    color: var(--muted);
    cursor: pointer;
    padding: 0.05rem 0.35rem;
    border-radius: 4px;
    text-transform: none;
    letter-spacing: 0;
  }
  .gt-action:hover {
    background: var(--row-hover);
    color: var(--fg);
  }

  .compose {
    padding: 0.25rem 0.7rem 0.4rem;
  }
  .compose-row {
    display: flex;
    align-items: center;
    gap: 0.35rem;
  }
  .seg {
    flex: none;
    display: flex;
    gap: 1px;
    background: var(--edge);
    border-radius: 5px;
    overflow: hidden;
  }
  .seg-btn {
    appearance: none;
    border: none;
    background: var(--term-bg);
    font: inherit;
    font-size: 0.62rem;
    color: var(--muted);
    cursor: pointer;
    padding: 0.14rem 0.4rem;
  }
  .seg-btn.on {
    background: var(--row-active);
    color: var(--fg);
  }
  .compose-input {
    flex: 1;
    min-width: 0;
    appearance: none;
    background: var(--term-bg);
    border: 1px solid var(--edge);
    border-radius: 5px;
    padding: 0.16rem 0.4rem;
    font-family: var(--mono);
    font-size: 0.72rem;
    color: var(--fg);
  }
  .compose-input:focus {
    outline: none;
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
  }
  .compose-actions {
    display: flex;
    gap: 0.35rem;
    margin-top: 0.3rem;
  }
  .compose-go,
  .compose-cancel {
    appearance: none;
    border: 1px solid var(--edge);
    background: var(--term-bg);
    font: inherit;
    font-size: 0.68rem;
    color: var(--fg);
    cursor: pointer;
    padding: 0.14rem 0.55rem;
    border-radius: 5px;
  }
  .compose-go {
    border-color: color-mix(in srgb, var(--accent) 45%, var(--edge));
    color: var(--accent);
  }
  .compose-go:disabled {
    opacity: 0.5;
    cursor: default;
    color: var(--muted);
    border-color: var(--edge);
  }
  .compose-go:not(:disabled):hover,
  .compose-cancel:hover {
    background: var(--row-hover);
  }

  .wt-error {
    margin: 0.1rem 0.7rem 0.3rem;
    padding: 0.2rem 0.4rem;
    font-size: 0.66rem;
    color: var(--git-deleted);
    background: color-mix(in srgb, var(--git-deleted) 10%, transparent);
    border-radius: 4px;
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
