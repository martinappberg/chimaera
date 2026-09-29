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
    keepOf,
    fetchGitBranches,
    fetchGitWorktrees,
    gitSectionOpen,
    notifyWorkspacesChanged,
    refreshGit,
    rememberGitSection,
    removeWorktree,
    type DiffMode,
    type GitBranch,
    type GitCommit,
    type GitEntry,
    type GitStatus,
    type GitWorktree,
  } from "./git";
  import { decoFor } from "./gitDeco";
  import { relTime } from "./gitFormat";
  import { midTruncate } from "../previews/files";
  import { baseName, createSession, dotState, dotTitle, displayName, type Session } from "./sessions";
  import Chevron from "../shared/Chevron.svelte";
  import FileIcon from "../shared/FileIcon.svelte";
  import SessionGlyph from "../shared/SessionGlyph.svelte";
  import GitHistoryList from "./GitHistoryList.svelte";

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

  // Sections under Changes: Branches (open by default) and History (closed
  // until asked for), remembered per workspace and repository.
  const sectionKey = $derived(repo ?? ".");
  let branchesOpen = $state(true);
  let historyOpen = $state(false);
  let otherOpen = $state(false);
  $effect(() => {
    branchesOpen = gitSectionOpen(wsId, `${sectionKey}:branches`, true);
    historyOpen = gitSectionOpen(wsId, `${sectionKey}:history`, false);
  });
  function toggleBranches(): void {
    branchesOpen = !branchesOpen;
    rememberGitSection(wsId, `${sectionKey}:branches`, branchesOpen);
  }
  function toggleHistory(): void {
    historyOpen = !historyOpen;
    rememberGitSection(wsId, `${sectionKey}:history`, historyOpen);
  }

  // Branches: each worktree of the repo (the main checkout first), and which
  // sessions live in it. The agent↔branch edge is the daemon's: each session
  // row names the checkout its current folder is in (`session.git.worktree`
  // — a shell's cwd, an agent's hook-reported cwd), so an agent that moved
  // into a worktree mid-session is listed there. Nothing is stored.
  const mainBranch = $derived(worktrees[0]?.branch ?? null);
  const allBranches = $derived(
    worktrees.map((wt, i) => ({
      wt,
      main: i === 0,
      sessions: [...sessions.values()]
        .filter((s) => s.alive)
        .filter((s) => s.git?.worktree === wt.path),
    })),
  );
  // Only what you can act on: the main checkout, the one you're in, any
  // holding sessions, and any Chimaera created (managed — removable here). A
  // repo can carry dozens of the user's own stale worktrees; listing them all
  // is chrome that hasn't earned its pixels, so the rest fold into a count.
  const branches = $derived(
    allBranches.filter((b) => b.main || b.wt.current || b.sessions.length > 0 || b.wt.managed),
  );
  const otherWorktrees = $derived(allBranches.length - branches.length);

  // Local branches with no worktree: read-only, under "Other branches".
  let localBranches = $state<GitBranch[]>([]);
  $effect(() => {
    if (!branchesOpen || wsId === null) return;
    const id = wsId;
    const top = repo;
    void status.repo_epoch;
    void status.epoch;
    let live = true;
    void fetchGitBranches(id, top ?? undefined)
      .then((r) => {
        if (live) localBranches = r.branches ?? [];
      })
      .catch(() => {
        if (live) localBranches = [];
      });
    return () => {
      live = false;
    };
  });
  const checkedOut = $derived(new Set(worktrees.map((w) => w.branch).filter((b) => b !== null)));
  const otherBranches = $derived(localBranches.filter((b) => !checkedOut.has(b.name)));

  function branchName(wt: GitWorktree): string {
    if (wt.branch) return wt.branch;
    return wt.detached ? `No branch (at ${wt.head ?? "?"})` : "No commits yet";
  }


  function openDiff(e: MouseEvent, entry: GitEntry, mode: DiffMode): void {
    ctrl.openDiffFrom(paneId, entry.path, mode, e.metaKey || e.ctrlKey, e.detail >= 2);
  }

  /** A branch row: "Changes on this branch" beside the panel. */
  function openBranch(e: MouseEvent, wt: GitWorktree, main: boolean): void {
    ctrl.openGitFrom(
      paneId,
      {
        surface: "gitx",
        view: "branch",
        // The main checkout is the repository itself; another worktree is
        // named by its folder (the daemon validates it as this repo's).
        repo: main ? repo : wt.path,
        title: wt.branch ?? baseName(wt.path),
        ...keepOf(e),
      },
      e.metaKey || e.ctrlKey,
    );
  }

  /** A local branch without a worktree: its history. */
  function openBranchHistory(e: MouseEvent, b: GitBranch): void {
    ctrl.openGitFrom(
      paneId,
      { surface: "gitx", view: "history", repo, rev: b.name, title: b.name, ...keepOf(e) },
      e.metaKey || e.ctrlKey,
    );
  }

  function openCommit(c: GitCommit, e: MouseEvent): void {
    ctrl.openGitFrom(
      paneId,
      { surface: "gitx", view: "commit", repo, sha: c.sha, title: c.subject, ...keepOf(e) },
      e.metaKey || e.ctrlKey,
    );
  }

  function dragCommit(c: GitCommit, e: PointerEvent): void {
    ctrl.beginGitDrag(
      e,
      { surface: "gitx", view: "commit", repo, sha: c.sha, title: c.subject },
      () => openCommit(c, e as unknown as MouseEvent),
    );
  }

  // ---- worktree orchestration (the panel's only mutations) ------------------

  // The composer: a name, where it starts, and whether an agent starts there.
  // Chimaera creates the worktree (and the agent). Collapsed until "+ New
  // branch" is clicked; it lives in a repository's own section, so which
  // repository is never in doubt.
  let composing = $state(false);
  let newBranch = $state("");
  let startAgent = $state(true);
  let busy = $state(false);
  let actionError = $state<string | null>(null);
  let copiedNote = $state<string | null>(null);
  // A note answers the action just taken ("Copied .env", "Worktree removed ·
  // branch kept"); it goes on its own after a few seconds.
  $effect(() => {
    if (copiedNote === null) return;
    const t = setTimeout(() => (copiedNote = null), 8000);
    return () => clearTimeout(t);
  });
  let branchInput = $state<HTMLInputElement | null>(null);
  // Where a NEW branch starts: one of the local branches ("" = the current
  // HEAD, what git does by default). Fetched when the composer opens.
  let baseChoices = $state<GitBranch[]>([]);
  let newBase = $state("");

  function startCompose(): void {
    composing = true;
    actionError = null;
    copiedNote = null;
    newBase = "";
    if (!branchesOpen) toggleBranches();
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

  function copiedWords(names: string[], copied: number): string | null {
    if (copied <= 0) return null;
    const first = names[0] ?? "a file";
    return copied === 1 ? `Copied ${first}` : `Copied ${first} and ${copied - 1} more`;
  }

  async function spawnInNewBranch(): Promise<void> {
    const branch = newBranch.trim();
    if (busy || wsId === null || branch === "") return;
    busy = true;
    actionError = null;
    try {
      const created = await createWorktree(wsId, branch, newBase || undefined, repo ?? undefined);
      copiedNote = copiedWords(created.included?.names ?? [], created.included?.copied ?? 0);
      refreshGit();
      composing = false;
      newBranch = "";
      if (startAgent) {
        // The branch is where the agent works, not where the window goes: it
        // starts in THIS workspace with the new worktree as its folder, and
        // the file tree, panes and everything else stay where they are.
        const session = await createSession(wsId, "agent", null, null, { cwd: created.worktree.path });
        onOpenSession(session.id, wsId);
      }
    } catch (e) {
      actionError = e instanceof Error ? e.message : "failed to create the worktree";
    } finally {
      busy = false;
    }
  }

  /** The worktree whose removal is being confirmed inline (its path). The
   *  confirmation lives in the row itself: the native app's web view has no
   *  `window.confirm` (it silently answers "no"), and an inline question is
   *  quieter than a dialog anyway. */
  let confirmingRemove = $state<string | null>(null);

  async function remove(wt: GitWorktree): Promise<void> {
    if (busy || wsId === null) return;
    busy = true;
    actionError = null;
    try {
      await removeWorktree(wsId, wt.path, false, repo ?? undefined);
      confirmingRemove = null;
      copiedNote = "Worktree removed · branch kept";
      notifyWorkspacesChanged();
      refreshGit();
    } catch (e) {
      confirmingRemove = null;
      actionError = e instanceof Error ? e.message : "failed to remove the worktree";
    } finally {
      busy = false;
    }
  }

  /** A worktree's folder name, only when it says something the branch name
   *  doesn't (chimaera names its worktrees after their branch). */
  function folderIfDifferent(wt: GitWorktree): string | null {
    const branch = wt.branch ?? "";
    if (branch !== "" && (wt.path === branch || wt.path.endsWith(`/${branch}`))) return null;
    return baseName(wt.path);
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
    <div class="sec">
      <button class="sec-toggle" aria-expanded={branchesOpen} onclick={toggleBranches}>
        <Chevron open={branchesOpen} size={9} />
        <span>Branches</span>
      </button>
      <span class="spacer"></span>
      <button class="gt-action" title="a new branch in its own worktree" onclick={startCompose}>
        + New branch
      </button>
    </div>

    {#if branchesOpen}
      {#if composing}
        <!-- Create a worktree for a new branch (and, by choice, an agent in it). -->
        <div class="compose">
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
          <label class="compose-base">
            <span class="compose-base-label">From</span>
            <select class="compose-select" bind:value={newBase} disabled={busy}>
              <option value="">{status.branch ?? "HEAD"} (current)</option>
              {#each baseChoices.filter((b) => !b.current) as b (b.name)}
                <option value={b.name}>{b.name}</option>
              {/each}
            </select>
          </label>
          <label class="compose-check">
            <input type="checkbox" bind:checked={startAgent} disabled={busy} />
            <span>Start an agent here</span>
          </label>
          <div class="compose-actions">
            <button
              class="compose-go"
              disabled={busy || newBranch.trim() === ""}
              onclick={() => void spawnInNewBranch()}>{busy ? "Creating…" : "Create"}</button>
            <button class="compose-cancel" disabled={busy} onclick={() => (composing = false)}>Cancel</button>
          </div>
        </div>
      {/if}
      {#if actionError !== null}
        <div class="wt-error" role="alert">{actionError}</div>
      {/if}
      {#if copiedNote !== null}
        <div class="wt-note-line">{copiedNote}</div>
      {/if}

      {#each branches as b (b.wt.path)}
        {@const removable = b.wt.managed && !b.wt.current && b.sessions.length === 0}
        {@const ahead = b.wt.ahead_of_main ?? 0}
        <div class="wt" class:current={b.wt.current}>
          <button
            class="wt-head"
            title={`${b.wt.path} — changes on this branch${b.wt.locked ? " · locked while an agent works here" : ""}`}
            onclick={(e) => openBranch(e, b.wt, b.main)}
          >
            <span class="wt-branch" class:detached={b.wt.detached}>{branchName(b.wt)}</span>
            {#if !b.main}
              {@const folder = folderIfDifferent(b.wt)}
              {#if folder !== null}<span class="wt-folder">{folder}</span>{/if}
            {/if}
            {#if !b.main && ahead > 0 && mainBranch}
              <span class="wt-note">{ahead} ahead of {mainBranch}</span>
            {/if}
            {#if removable && b.wt.merged && confirmingRemove !== b.wt.path}<span class="wt-note">merged</span>{/if}
          </button>
          {#each b.sessions as s (s.id)}
            <button
              class="wt-agent"
              title={`${names.get(s.id) ?? displayName(s)} — ${dotTitle(s)}`}
              onclick={() => onOpenSession(s.id, s.workspace_id)}
            >
              <SessionGlyph kind={s.kind} agentKind={s.agent_kind} state={dotState(s)} size={11} />
            </button>
          {/each}
          <!-- Remove only where the daemon would allow it: a managed worktree
               that is neither the current one nor holding sessions. A merged
               one keeps the action visible — it's done. -->
          {#if removable}
            {#if confirmingRemove === b.wt.path}
              <span class="wt-confirm" role="group" aria-label="Remove this worktree? The branch stays.">
                <span class="wt-confirm-q">Remove worktree?</span>
                <button
                  class="wt-remove offered strong"
                  title="delete this worktree's folder — the branch stays"
                  disabled={busy}
                  onclick={() => void remove(b.wt)}>Remove</button>
                <button class="wt-remove offered" disabled={busy} onclick={() => (confirmingRemove = null)}>Keep</button>
              </span>
            {:else}
              <button
                class="wt-remove"
                class:offered={b.wt.merged === true}
                title="remove this worktree's folder (the branch is kept)"
                disabled={busy}
                onclick={() => {
                  actionError = null;
                  confirmingRemove = b.wt.path;
                }}>Remove worktree</button>
            {/if}
          {/if}
        </div>
      {/each}
      {#if otherWorktrees > 0}
        <div class="wt-more">
          {otherWorktrees} other worktree{otherWorktrees === 1 ? "" : "s"}, no sessions
        </div>
      {/if}
      {#if otherBranches.length > 0}
        <div class="other">
          <button class="sec-toggle sub" aria-expanded={otherOpen} onclick={() => (otherOpen = !otherOpen)}>
            <Chevron open={otherOpen} size={9} />
            <span>Other branches ({otherBranches.length})</span>
          </button>
          {#if otherOpen}
            {#each otherBranches as b (b.name)}
              <button class="ob" title={`${b.name} — its history`} onclick={(e) => openBranchHistory(e, b)}>
                <span class="ob-name">{b.name}</span>
                <span class="ob-time">{relTime(b.time)}</span>
              </button>
            {/each}
          {/if}
        </div>
      {/if}
    {/if}
  </div>
{/if}

<div class="group history">
  <div class="sec">
    <button class="sec-toggle" aria-expanded={historyOpen} onclick={toggleHistory}>
      <Chevron open={historyOpen} size={9} />
      <span>History</span>
    </button>
  </div>
  {#if historyOpen}
    <GitHistoryList
      {wsId}
      {repo}
      initial={20}
      refreshKey={repo === null ? status.epoch : status.repo_epoch}
      onOpen={openCommit}
      onDragStart={dragCommit}
    />
  {/if}
</div>

<style>
  .spacer {
    flex: 1;
  }
  .detached {
    color: var(--warn);
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

  /* Branches and History: collapsible sections under Changes. */
  .branches,
  .history {
    margin-top: 0.35rem;
    border-top: 1px solid var(--edge);
    padding-top: 0.2rem;
  }
  .sec {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    padding: 0.2rem 0.7rem 0.15rem 0.45rem;
  }
  .sec-toggle {
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    appearance: none;
    border: none;
    background: none;
    padding: 0.1rem 0.2rem;
    border-radius: 4px;
    font: inherit;
    font-size: var(--text-xs);
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--muted);
    cursor: pointer;
  }
  .sec-toggle:hover {
    color: var(--fg);
  }
  .sec-toggle.sub {
    text-transform: none;
    letter-spacing: 0;
    padding-left: 0.45rem;
  }

  /* One row per worktree branch: name, folder, how far ahead, the agents
     working there, and — when done — a quiet way to remove it. */
  .wt {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    padding: 0 0.7rem 0 0.45rem;
    min-height: calc(var(--text-sm) + 9px);
  }
  .wt:hover {
    background: var(--row-hover);
  }
  .wt-head {
    flex: 1 1 auto;
    min-width: 0;
    display: flex;
    align-items: baseline;
    gap: 0.45rem;
    appearance: none;
    border: none;
    background: none;
    padding: 0.15rem 0.25rem;
    font: inherit;
    text-align: left;
    color: var(--muted);
    cursor: pointer;
  }
  .wt-head:focus-visible {
    outline: 1px solid var(--focus-ring);
    outline-offset: -1px;
  }
  .wt-branch {
    font-family: var(--mono);
    font-size: var(--text-sm);
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }
  .wt.current .wt-branch,
  .wt-head:hover .wt-branch {
    color: var(--fg);
  }
  .wt-folder,
  .wt-note {
    flex: none;
    font-size: var(--text-xs);
    color: var(--muted);
    opacity: 0.85;
    white-space: nowrap;
  }
  .wt-folder {
    font-family: var(--mono);
    opacity: 0.65;
  }
  .wt-agent {
    flex: none;
    display: inline-flex;
    appearance: none;
    border: none;
    background: none;
    padding: 0.1rem;
    border-radius: 4px;
    cursor: pointer;
  }
  .wt-agent:hover {
    background: var(--row-active);
  }
  .wt-remove {
    flex: none;
    appearance: none;
    border: none;
    background: none;
    padding: 0.05rem 0.2rem;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: pointer;
    border-radius: 3px;
    opacity: 0;
    transition: opacity 0.1s ease;
  }
  .wt:hover .wt-remove,
  .wt-remove.offered {
    opacity: 0.85;
  }
  .wt-remove:hover {
    opacity: 1;
    color: var(--fg);
    text-decoration: underline;
    text-underline-offset: 2px;
  }
  .wt-remove:focus-visible {
    opacity: 1;
    outline: 1px solid var(--focus-ring);
  }
  .wt-remove.strong {
    color: var(--fg);
  }
  .wt-confirm {
    flex: none;
    display: inline-flex;
    align-items: baseline;
    gap: 0.35rem;
    font-size: var(--text-xs);
  }
  .wt-confirm-q {
    color: var(--muted);
  }

  .wt-more,
  .wt-note-line {
    padding: 0.15rem 0.7rem 0.2rem 0.95rem;
    font-size: var(--text-xs);
    color: var(--muted);
    opacity: 0.8;
  }

  .other {
    padding-top: 0.1rem;
  }
  .ob {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    width: 100%;
    min-height: calc(var(--text-sm) + 7px);
    padding: 0 0.7rem 0 1.6rem;
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    text-align: left;
    cursor: pointer;
    color: var(--muted);
  }
  .ob:hover {
    background: var(--row-hover);
    color: var(--fg);
  }
  .ob-name {
    flex: 1;
    min-width: 0;
    font-family: var(--mono);
    font-size: var(--text-sm);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .ob-time {
    flex: none;
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
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
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    padding: 0.25rem 0.7rem 0.45rem 0.95rem;
  }
  .compose-check {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    font-size: var(--text-xs);
    color: var(--muted);
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
  }
  .compose-base {
    display: flex;
    align-items: center;
    gap: 0.35rem;
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
