<script lang="ts">
  /**
   * One repository's body in the Source Control panel: its changes grouped by
   * staged / changes / untracked / conflicts (click-to-diff), and its branches
   * (the worktrees, the sessions in each, the "+ branch" composer). With one
   * repository in the workspace this IS the panel below its header — exactly
   * what it always was; with several, each repository row expands to one.
   *
   * `repo` names the repository to the daemon (its top level); null is the
   * workspace's own, the routes' default.
   */
  import type { LayoutCtrl } from "../layout/dnd";
  import {
    createWorktree,
    fetchGitBranches,
    fetchGitWorktrees,
    notifyWorkspacesChanged,
    refreshGit,
    removeWorktree,
    type DiffMode,
    type GitBranch,
    type GitEntry,
    type GitStatus,
    type GitWorktree,
  } from "./git";
  import { decoFor } from "./gitDeco";
  import { midTruncate } from "../previews/files";
  import { createSession, type Session, type SessionKind } from "./sessions";
  import FileIcon from "../shared/FileIcon.svelte";
  import SessionGlyph from "../shared/SessionGlyph.svelte";

  interface Props {
    wsId: string | null;
    paneId: string;
    ctrl: LayoutCtrl;
    sessions: Map<string, Session>;
    names: Map<string, string>;
    onOpenSession: (sessionId: string, workspaceId: string) => void;
    /** The repository's top level; null = the workspace's own. */
    repo: string | null;
    status: GitStatus;
    /** The primary's worktrees come from the store; others are fetched here. */
    worktrees?: GitWorktree[];
    /** Top levels of repositories inside this one: their folders are links
     *  to their own sections, not changes. */
    nestedRepos?: string[];
    onOpenRepo?: (path: string) => void;
  }
  let {
    wsId,
    paneId,
    ctrl,
    sessions,
    names,
    onOpenSession,
    repo,
    status,
    worktrees: givenWorktrees,
    nestedRepos = [],
    onOpenRepo,
  }: Props = $props();

  const bare = (p: string): string => (p.endsWith("/") ? p.slice(0, -1) : p);
  const nestedSet = $derived(new Set(nestedRepos));
  const isLink = (e: GitEntry): boolean =>
    nestedSet.has(bare(e.path)) && (e.untracked || e.submodule === true);

  const allEntries = $derived(status.entries ?? []);
  // A nested repository shows as one untracked folder (or a submodule entry)
  // in the outer one: a link to its section, not a change.
  const links = $derived(allEntries.filter(isLink));
  const entries = $derived(allEntries.filter((e) => !isLink(e)));

  // The daemon couldn't read this repository (it answered with an error):
  // most often git refusing a repo another user owns on shared storage — the
  // remedy is one command, per repository.
  const dubiousPath = $derived.by(() => {
    if (!status.error) return null;
    const m = /dubious ownership in repository at '([^']+)'/.exec(status.error);
    return m ? m[1] : null;
  });

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

  const clean = $derived(entries.length === 0 && !status.error);

  // Worktrees: the primary's ride the store; another repository's are asked
  // for here, again whenever its status moves (a checkout elsewhere shows).
  let ownWorktrees = $state<GitWorktree[]>([]);
  $effect(() => {
    if (givenWorktrees !== undefined || wsId === null || repo === null) return;
    const id = wsId;
    const top = repo;
    void status.repo_epoch;
    let live = true;
    void fetchGitWorktrees(id, top)
      .then((r) => {
        if (live) ownWorktrees = r.worktrees ?? [];
      })
      .catch(() => {
        if (live) ownWorktrees = [];
      });
    return () => {
      live = false;
    };
  });
  const worktrees = $derived(givenWorktrees ?? ownWorktrees);

  // Branches: each worktree of the repo, and which sessions live in it. The
  // agent↔branch edge is the daemon's: each session row names the checkout
  // its current folder is in (`session.git.worktree` — a shell's cwd, an
  // agent's hook-reported cwd), so an agent that moved into a worktree
  // mid-session is listed there. Nothing is stored.
  // A single-worktree repo shows no rows here: the header already names the
  // branch, and an empty section is chrome that hasn't earned its pixels.
  const allBranches = $derived(
    worktrees.length < 2
      ? []
      : worktrees.map((wt) => ({
          wt,
          sessions: [...sessions.values()]
            .filter((s) => s.alive)
            .filter((s) => s.git?.worktree === wt.path),
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
  // "+ branch" affordance is clicked so the panel stays quiet. It lives in a
  // repository's own section, so which repository is never in doubt.
  let composing = $state(false);
  let newBranch = $state("");
  let newKind = $state<SessionKind>("agent");
  let busy = $state(false);
  let actionError = $state<string | null>(null);
  let branchInput = $state<HTMLInputElement | null>(null);
  // Where a NEW branch starts: one of the local branches ("" = the current
  // HEAD, what git does by default). Fetched when the composer opens.
  let baseChoices = $state<GitBranch[]>([]);
  let newBase = $state("");

  function startCompose(): void {
    composing = true;
    actionError = null;
    newBase = "";
    if (wsId !== null) {
      const id = wsId;
      void fetchGitBranches(id, repo ?? undefined)
        .then((r) => {
          if (wsId === id) baseChoices = r.branches ?? [];
        })
        .catch(() => {
          baseChoices = [];
        });
    }
    void Promise.resolve().then(() => branchInput?.focus());
  }

  async function spawnInNewBranch(): Promise<void> {
    const branch = newBranch.trim();
    if (busy || wsId === null || branch === "") return;
    busy = true;
    actionError = null;
    try {
      const created = await createWorktree(wsId, branch, newBase || undefined, repo ?? undefined);
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
      await removeWorktree(wsId, wt.path, false, repo ?? undefined);
      notifyWorkspacesChanged();
      refreshGit();
    } catch (e) {
      actionError = e instanceof Error ? e.message : "failed to remove the worktree";
    } finally {
      busy = false;
    }
  }
</script>

{#if dubiousPath}
  <div class="gitenv">
    <div class="gitenv-title">Couldn’t read this repository</div>
    <p class="gitenv-hint">
      Git refuses repos it thinks another user owns (common on shared
      cluster storage). If this checkout is yours, mark it trusted:
    </p>
    <p class="gitenv-where">
      <span class="mono">git config --global --add safe.directory {dubiousPath}</span>
    </p>
    <p class="gitenv-hint">Then refresh.</p>
  </div>
{:else if status.error && entries.length === 0}
  <div class="empty" title={status.error}>Status unavailable right now.</div>
{:else if clean && links.length === 0}
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
  {#if links.length > 0}
    <!-- Repositories inside this one: each is its own section. -->
    <div class="group">
      <div class="gtitle"><span>Repositories inside</span><span class="gcount">{links.length}</span></div>
      {#each links as entry (entry.path)}
        <button
          class="grow link"
          title={`${bare(entry.path)} — its own repository`}
          onclick={() => onOpenRepo?.(bare(entry.path))}
        >
          <span class="gicon">→</span>
          <span class="gname">{midTruncate(bare(entry.rel), 58)}</span>
        </button>
      {/each}
    </div>
  {/if}
  {#if clean}
    <div class="empty">Working tree clean.</div>
  {/if}
  {#if status.truncated}
    <div class="trunc">Too many changes to list them all.</div>
  {/if}
{/if}

{#if worktrees.length >= 1}
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
        {#if baseChoices.length > 0}
          <!-- A new branch starts from here; an existing branch name is
               checked out as it is (git's own rule), whatever this says. -->
          <label class="compose-base">
            <span class="compose-base-label">from</span>
            <select class="compose-select" bind:value={newBase} disabled={busy}>
              <option value="">{status.branch ?? "HEAD"} (current)</option>
              {#each baseChoices.filter((b) => !b.current) as b (b.name)}
                <option value={b.name}>{b.name}</option>
              {/each}
            </select>
          </label>
        {/if}
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
      {@const removable = b.wt.managed && !b.wt.current && b.sessions.length === 0}
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
          {#if b.wt.locked}<span class="wt-tag muted" title="locked — other tools' clean-up leaves it alone">locked</span>{/if}
          {#if b.wt.prunable}<span class="wt-tag muted">prunable</span>{/if}
          {#if removable && b.wt.merged}
            <span
              class="wt-tag muted"
              title={`${status.branch ?? "the main checkout"} already contains this branch`}>merged</span>
          {/if}
          {#if b.sessions.length > 0}
            <span class="wt-count">{b.sessions.length}</span>
          {/if}
          <!-- Remove only where the daemon would allow it: a managed
               worktree that is neither the current one nor holding sessions.
               A merged one keeps the control visible — it's done. -->
          {#if removable}
            <button
              class="wt-remove"
              class:offered={b.wt.merged === true}
              title={b.wt.merged
                ? "merged — remove this worktree (keeps the branch)"
                : "remove this worktree (keeps the branch)"}
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

<style>
  .spacer {
    flex: 1;
  }
  .detached {
    color: var(--warn);
  }
  .sha {
    opacity: 0.8;
  }

  .group {
    margin-bottom: 0.35rem;
  }

  .gtitle {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    padding: 0.3rem 0.7rem 0.2rem;
    font-size: var(--text-xs);
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
    min-height: calc(var(--text-sm) + 9px);
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
  .grow.link .gicon {
    width: 13px;
    justify-content: center;
    font-size: var(--text-xs);
  }

  .gicon {
    flex: none;
    display: flex;
    align-items: center;
  }

  .gname {
    font-family: var(--mono);
    font-size: var(--text-sm);
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
    font-size: var(--text-sm);
  }

  .gbadge {
    flex: none;
    margin-left: auto;
    font-family: var(--mono);
    font-size: var(--text-xs);
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
    min-height: calc(var(--text-sm) + 7px);
  }

  .wt-branch {
    font-family: var(--mono);
    font-size: var(--text-sm);
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
    font-size: var(--text-xs);
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
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    color: var(--muted);
  }

  .wt-session {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    width: 100%;
    padding: 0 0.7rem 0 0.9rem;
    min-height: calc(var(--text-sm) + 7px);
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
    font-size: var(--text-sm);
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
    font-size: var(--text-lg);
    line-height: 1;
    padding: 0 0.15rem;
    border-radius: 3px;
    opacity: 0;
    transition:
      opacity 0.1s ease,
      color 0.1s ease,
      background-color 0.1s ease;
  }
  .wt:hover .wt-remove,
  .wt-remove.offered {
    opacity: 0.7;
  }
  .wt-remove:hover {
    opacity: 1;
    color: var(--git-deleted);
    background: var(--row-hover);
  }

  .wt-more {
    padding: 0.15rem 0.7rem 0.2rem;
    font-size: var(--text-xs);
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
    font-size: var(--text-xs);
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
    font-size: var(--text-xs);
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
    font-size: var(--text-sm);
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
  .compose-base {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    margin-top: 0.3rem;
    min-width: 0;
  }
  .compose-base-label {
    flex: none;
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .compose-select {
    flex: 1;
    min-width: 0;
    appearance: auto;
    background: var(--term-bg);
    color: var(--fg);
    border: 1px solid var(--edge);
    border-radius: 5px;
    padding: 0.1rem 0.3rem;
    font-family: var(--mono);
    font-size: var(--text-xs);
  }
  .compose-select:focus {
    outline: none;
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
  }
  .compose-go,
  .compose-cancel {
    appearance: none;
    border: 1px solid var(--edge);
    background: var(--term-bg);
    font: inherit;
    font-size: var(--text-xs);
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
    font-size: var(--text-xs);
    color: var(--git-deleted);
    background: color-mix(in srgb, var(--git-deleted) 10%, transparent);
    border-radius: 4px;
  }

  .gitenv {
    padding: 0.6rem 0.85rem 0.7rem;
    display: flex;
    flex-direction: column;
    gap: 0.45rem;
  }
  .gitenv-title {
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--fg);
  }
  .gitenv-where,
  .gitenv-hint {
    margin: 0;
    font-size: var(--text-sm);
    line-height: 1.5;
    color: var(--muted);
  }
  .gitenv .mono {
    display: inline-block;
    max-width: 100%;
    white-space: normal;
    overflow-wrap: anywhere;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--fg);
    background: var(--row-active);
    padding: 0.03rem 0.28rem;
    border-radius: 4px;
  }

  .empty,
  .trunc {
    padding: 1rem 0.8rem;
    font-size: var(--text-sm);
    color: var(--muted);
    text-align: center;
  }
  .trunc {
    padding: 0.5rem 0.8rem;
    opacity: 0.8;
  }
</style>
