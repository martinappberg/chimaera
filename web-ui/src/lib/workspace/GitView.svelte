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
  import type { LayoutCtrl } from "../layout/dnd";
  import {
    aheadBehindWords,
    gitEnv,
    gitExpandedRepos,
    gitFocus,
    gitRevealRepo,
    repoForPath,
    gitRepoError,
    gitRepos,
    gitReposCapped,
    gitRepoStatuses,
    gitStatus,
    gitWorktrees,
    refreshGit,
    setRepoExpanded,
    type GitRepo,
    type GitStatus,
  } from "./git";
  import { flushSettings, getSetting, setSetting } from "../settings/store.svelte";
  import { type Session } from "./sessions";
  import Chevron from "../shared/Chevron.svelte";
  import GitRepoSection from "./GitRepoSection.svelte";

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

  // The daemon's resolved git binary. When `ok` is false it is missing or too
  // old to drive the service (e.g. an HPC login node's git 1.8), so the panel
  // shows how to point chimaera at a modern git instead of a blank repo.
  const env = $derived($gitEnv);
  const gitBad = $derived(env !== null && env.ok === false);

  // Git resolved fine but couldn't READ this repo (dubious ownership on shared
  // storage, a permission problem, a timeout). Shown only when it isn't the
  // git-binary itself that's the problem, and there's genuinely no repo to
  // render — turning the flat "not a git repository" dead end into a fix.
  const repoError = $derived(!gitBad && status === null ? $gitRepoError : null);
  // The most common HPC cause is git refusing a repo it considers unsafe; when
  // that's it, offer the exact one-line remedy with the path git named.
  const dubiousPath = $derived.by(() => {
    if (repoError === null) return null;
    const m = /dubious ownership in repository at '([^']+)'/.exec(repoError);
    return m ? m[1] : null;
  });

  // Seed the path field from the current setting the first time the bad-git
  // state appears; the user edits from there (blank clears the override).
  let gitPathInput = $state("");
  let gitSeeded = false;
  let savingGit = $state(false);
  $effect(() => {
    if ((gitBad || repoError !== null) && !gitSeeded) {
      gitPathInput = getSetting("git.path");
      gitSeeded = true;
    }
  });

  function sourceLabel(source: string | undefined): string {
    return source === "setting"
      ? "the path you set"
      : source === "login-shell"
        ? "your login shell"
        : "the daemon's PATH";
  }

  async function saveGitPath(): Promise<void> {
    if (savingGit) return;
    savingGit = true;
    try {
      // "" removes the override (resolve from login shell / PATH). Flush now so
      // the daemon has the new value before we ask it to re-resolve.
      setSetting("git.path", gitPathInput.trim());
      await flushSettings();
      refreshGit();
    } finally {
      savingGit = false;
    }
  }

  // Several repositories (a folder of projects, a cloned tool inside one, a
  // submodule): the panel lists them, each expanding to its own changes and
  // branches. One repository looks exactly as it always did.
  const repos = $derived($gitRepos);
  const multi = $derived(
    repos.length > 1 ||
      (repos.length === 1 && repos[0].kind !== "root" && repos[0].kind !== "enclosing"),
  );
  const primaryTop = $derived(status?.toplevel ?? null);
  const belowRoot = $derived(
    repos.filter((r) => r.kind === "nested" || r.kind === "submodule").map((r) => r.path),
  );

  function statusOf(r: GitRepo): GitStatus | null {
    return r.kind === "root" || r.kind === "enclosing"
      ? status
      : ($gitRepoStatuses.get(r.path) ?? null);
  }

  function repoName(r: GitRepo): string {
    if (r.rel && r.rel !== ".") return r.rel;
    return r.path.split("/").filter(Boolean).pop() ?? r.path;
  }

  /** The repositories directly inside `top` (their folders are links). */
  function nestedIn(top: string | null): string[] {
    if (top === null) return belowRoot;
    return belowRoot.filter((p) => p !== top && p.startsWith(`${top}/`));
  }

  function openRepo(path: string): void {
    setRepoExpanded(path, true);
  }

  // The list: nesting shown by a small indent under the parent repository;
  // within each level, repositories with changes first, then by name.
  const rows = $derived.by(() => {
    const tops = new Set(repos.map((r) => r.path));
    const byParent = new Map<string | null, GitRepo[]>();
    for (const r of repos) {
      const parent = r.parent !== null && tops.has(r.parent) ? r.parent : null;
      byParent.set(parent, [...(byParent.get(parent) ?? []), r]);
    }
    const changed = (r: GitRepo): number => ((statusOf(r)?.counts?.total ?? 0) > 0 ? 1 : 0);
    const out: { repo: GitRepo; depth: number }[] = [];
    const walk = (parent: string | null, depth: number): void => {
      const level = [...(byParent.get(parent) ?? [])].sort(
        (a, b) => changed(b) - changed(a) || repoName(a).localeCompare(repoName(b)),
      );
      for (const r of level) {
        out.push({ repo: r, depth });
        if (depth < 8) walk(r.path, depth + 1);
      }
    };
    walk(null, 0);
    return out;
  });

  // The repository holding what's focused (a file, a session) opens by
  // itself; every other one stays a single line until clicked.
  let lastFocused: string | null = null;
  $effect(() => {
    if (!multi) return;
    const r = repoForPath(repos, $gitFocus);
    if (r !== null && r.path !== lastFocused) {
      lastFocused = r.path;
      setRepoExpanded(r.path, true);
    }
  });

  // The status chip's click: scroll to that repository's row.
  let rowEls: Record<string, HTMLElement | undefined> = $state({});
  $effect(() => {
    const req = $gitRevealRepo;
    if (req === null) return;
    const el = rowEls[req.path];
    if (el) void Promise.resolve().then(() => el.scrollIntoView({ block: "nearest" }));
  });
</script>

<div class="git-view">
  <header class="ghead">
    {#if gitBad}
      <span class="branch none warn">git {env?.version ? "too old" : "not found"}</span>
      <span class="spacer"></span>
      <button class="refresh" title="re-check git" onclick={() => refreshGit()}>
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
    {:else if repoError !== null}
      <span class="branch none warn">can’t read repo</span>
      <span class="spacer"></span>
      <button class="refresh" title="re-check git" onclick={() => refreshGit()}>
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
    {:else if multi}
      <span class="branch none">{repos.length} repositor{repos.length === 1 ? "y" : "ies"} in this folder</span>
      <span class="spacer"></span>
      <button class="refresh" title="look for repositories again and refresh" onclick={() => refreshGit()}>
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
    {:else if status === null}
      <!-- No repository: the body says so, and nothing else. -->
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
          <span class="detached">No branch</span>
          <span class="sha">(at {status.head ?? "?"})</span>
        {:else}
          {status.branch ?? "No commits yet"}
        {/if}
      </span>
      {#if status.ahead > 0 || status.behind > 0}
        <span class="ab" title={aheadBehindWords(status.ahead, status.behind, status.upstream)}
          >{#if status.ahead > 0}↑{status.ahead}{/if}{#if status.behind > 0}{status.ahead > 0 ? " " : ""}↓{status.behind}{/if}</span
        >
      {/if}
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
    {#if gitBad}
      <div class="gitenv">
        <div class="gitenv-title">
          {env?.version ? `Git ${env.version} is too old` : "No usable git found"}
        </div>
        <p class="gitenv-body">
          Source control (status, diffs, worktrees) needs <b>git ≥ {env?.min ?? "2.15"}</b>.
          {#if env?.version}
            The git here is <span class="mono">{env.version}</span> — from before
            porcelain-v2 and <span class="mono">worktree</span> existed.
          {/if}
        </p>
        <p class="gitenv-where">
          Looking at <span class="mono">{env?.path}</span>
          <span class="gitenv-src">({sourceLabel(env?.source)})</span>
        </p>
        <p class="gitenv-hint">
          On a cluster, load a newer git — e.g. <span class="mono">module load git</span> — then
          paste its path below (run <span class="mono">command&nbsp;-v&nbsp;git</span> to find it).
          Leave blank to resolve from your login shell.
        </p>
        <div class="gitenv-form">
          <input
            class="compose-input"
            bind:value={gitPathInput}
            placeholder="path to git ≥ {env?.min ?? '2.15'}"
            spellcheck="false"
            autocapitalize="off"
            autocorrect="off"
            disabled={savingGit}
            onkeydown={(e) => {
              if (e.key === "Enter") void saveGitPath();
            }}
          />
          <button
            class="compose-go"
            disabled={savingGit}
            onclick={() => void saveGitPath()}>{savingGit ? "checking…" : "use this git"}</button>
        </div>
      </div>
    {:else if repoError !== null}
      <div class="gitenv">
        <div class="gitenv-title">Couldn’t read this repository</div>
        <p class="gitenv-body">
          Git is fine, but it wouldn’t open the repo here. This is a real repo —
          it isn’t showing because git returned:
        </p>
        <p class="gitenv-where"><span class="mono err">{repoError}</span></p>
        {#if dubiousPath}
          <p class="gitenv-hint">
            Git refuses repos it thinks another user owns (common on shared
            cluster storage). If this checkout is yours, mark it trusted:
          </p>
          <p class="gitenv-where">
            <span class="mono">git config --global --add safe.directory {dubiousPath}</span>
          </p>
          <p class="gitenv-hint">Then re-check.</p>
        {:else}
          <p class="gitenv-hint">
            Often a permissions or filesystem issue in the checkout. If a
            different git would help (e.g. a newer one via
            <span class="mono">module load git</span>), point chimaera at it —
            leave blank to resolve from your login shell.
          </p>
          <div class="gitenv-form">
            <input
              class="compose-input"
              bind:value={gitPathInput}
              placeholder="path to git ≥ {env?.min ?? '2.15'}"
              spellcheck="false"
              autocapitalize="off"
              autocorrect="off"
              disabled={savingGit}
              onkeydown={(e) => {
                if (e.key === "Enter") void saveGitPath();
              }}
            />
            <button
              class="compose-go"
              disabled={savingGit}
              onclick={() => void saveGitPath()}>{savingGit ? "checking…" : "use this git"}</button>
          </div>
        {/if}
      </div>
    {:else if multi}
      {#each rows as { repo: r, depth } (r.path)}
        {@const st = statusOf(r)}
        {@const open = $gitExpandedRepos.has(r.path)}
        {@const count = st?.counts?.total ?? 0}
        <div class="repo" class:open bind:this={rowEls[r.path]}>
          <button
            class="repo-head"
            style:padding-left="calc(0.6rem + {depth * 0.8}rem)"
            title={r.path}
            aria-expanded={open}
            onclick={() => setRepoExpanded(r.path, !open)}
          >
            <Chevron {open} />
            <span class="repo-name">{repoName(r)}</span>
            {#if r.submodule}
              <span class="repo-sub" title="submodule" aria-label="submodule">
                <svg viewBox="0 0 16 16" width="10" height="10" aria-hidden="true">
                  <rect x="2.5" y="2.5" width="11" height="11" rx="2" fill="none" stroke="currentColor" stroke-width="1.3" />
                  <rect x="6" y="6" width="4" height="4" rx="0.8" fill="currentColor" />
                </svg>
              </span>
            {/if}
            <span class="repo-dot" aria-hidden="true">·</span>
            <span class="repo-branch" class:detached={r.detached}
              >{r.branch ?? (r.detached ? `No branch (at ${r.head ?? "?"})` : "No commits yet")}</span
            >
            {#if st && (st.ahead > 0 || st.behind > 0)}
              <span class="ab" title={aheadBehindWords(st.ahead, st.behind, st.upstream)}
                >{#if st.ahead > 0}↑{st.ahead}{/if}{#if st.behind > 0}{st.ahead > 0 ? " " : ""}↓{st.behind}{/if}</span
              >
            {/if}
            {#if st?.error}
              <span class="repo-dot" aria-hidden="true">·</span>
              <span class="gerr" title={st.error}>can’t read</span>
            {:else if count > 0}
              <span class="repo-dot" aria-hidden="true">·</span>
              <span class="repo-count">{count} changed</span>
            {/if}
          </button>
          {#if open && st}
            <div class="repo-body" style:padding-left="{0.55 + depth * 0.8}rem">
              <GitRepoSection
                {wsId}
                {paneId}
                {ctrl}
                {sessions}
                {names}
                {onOpenSession}
                repo={r.kind === "root" || r.kind === "enclosing" ? null : r.path}
                status={st}
                worktrees={r.kind === "root" || r.kind === "enclosing" ? $gitWorktrees : undefined}
                nestedRepos={nestedIn(r.kind === "root" || r.kind === "enclosing" ? primaryTop : r.path)}
                onOpenRepo={openRepo}
              />
            </div>
          {/if}
        </div>
      {/each}
      {#if $gitReposCapped}
        <div class="trunc">More repositories than listed — the rest appear as you open their folders.</div>
      {/if}
    {:else if status === null}
      <div class="empty">
        {wsId === null
          ? "Open a workspace to see its git state."
          : "This folder isn’t a git repository."}
      </div>
    {:else}
      <GitRepoSection
        {wsId}
        {paneId}
        {ctrl}
        {sessions}
        {names}
        {onOpenSession}
        repo={null}
        {status}
        worktrees={$gitWorktrees}
        nestedRepos={nestedIn(primaryTop)}
        onOpenRepo={openRepo}
      />
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
    min-height: calc(var(--text-sm) + 17px);
    padding: 0 0.6rem;
    border-bottom: 1px solid var(--edge);
    font-size: var(--text-sm);
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
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    color: var(--muted);
  }

  .gerr {
    color: var(--warn);
    font-size: var(--text-xs);
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
  .compose-go {
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
  .compose-go:not(:disabled):hover {
    background: var(--row-hover);
  }

  .branch.none.warn {
    color: var(--warn);
  }

  .gitenv {
    padding: 0.9rem 0.85rem 1rem;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .gitenv-title {
    font-size: var(--text-md);
    font-weight: 600;
    color: var(--fg);
  }
  .gitenv-body,
  .gitenv-where,
  .gitenv-hint {
    margin: 0;
    font-size: var(--text-sm);
    line-height: 1.5;
    color: var(--muted);
  }
  .gitenv-body b {
    color: var(--fg);
    font-weight: 600;
  }
  .gitenv-src {
    opacity: 0.75;
  }
  .gitenv .mono {
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--fg);
    background: var(--row-active);
    padding: 0.03rem 0.28rem;
    border-radius: 4px;
    white-space: nowrap;
  }
  /* The remedy command and the raw git error can be long — let them wrap and
     break rather than overflow the narrow panel. */
  .gitenv-where .mono {
    display: inline-block;
    max-width: 100%;
    white-space: normal;
    overflow-wrap: anywhere;
  }
  .gitenv .mono.err {
    color: var(--warn);
    background: color-mix(in srgb, var(--warn) 12%, transparent);
  }
  .gitenv-form {
    display: flex;
    gap: 0.35rem;
    margin-top: 0.15rem;
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
  /* Several repositories: one row each, expanding to its own section. */
  .repo {
    border-bottom: 1px solid color-mix(in srgb, var(--edge) 60%, transparent);
  }
  .repo-head {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    width: 100%;
    min-height: calc(var(--text-sm) + 11px);
    padding: 0 0.6rem;
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    text-align: left;
    cursor: pointer;
    color: var(--muted);
  }
  .repo-head:hover {
    background: var(--row-hover);
  }
  .repo-head:focus-visible {
    outline: 1px solid var(--focus-ring);
    outline-offset: -1px;
  }
  .repo-name {
    font-family: var(--mono);
    font-size: var(--text-sm);
    color: var(--fg);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }
  .repo-sub {
    flex: none;
    display: inline-flex;
    color: var(--muted);
    opacity: 0.8;
  }
  .repo-dot {
    flex: none;
    color: var(--muted);
    opacity: 0.6;
  }
  .repo-branch {
    flex: 0 1 auto;
    min-width: 0;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .repo-branch.detached {
    color: var(--warn);
  }
  .repo-count {
    flex: none;
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    color: var(--muted);
  }
  .repo-body {
    padding-bottom: 0.3rem;
  }

</style>
