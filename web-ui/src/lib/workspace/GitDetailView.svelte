<script lang="ts">
  /**
   * The git history surfaces (one pane-tab kind, `gitx`):
   *
   * - **commit** — the subject, the body as plain text, `author · date · sha`
   *   (the sha copies), then the files it changed with small +/− counts; a
   *   file opens its diff (that commit against its parent) beside this pane.
   *   A quiet "Reference in chat"; dragging the tab to a chat works too.
   * - **history** — one file's commits (renames followed), or a branch's.
   * - **branch** — "Changes on this branch": everything since the branch left
   *   its base, uncommitted work included; a file opens its diff against the
   *   point the branch left from.
   *
   * Read-only, like all of git here: nothing checks out, reverts or resets.
   */
  import type { LayoutCtrl } from "../layout/dnd";
  import type { GitDetailTab } from "../layout/layout";
  import {
    fetchGitCompare,
    fetchGitShow,
    gitRepoStatuses,
    gitRepos,
    gitStatus,
    repoForPath,
    type GitBranchChanges,
    type GitChangedFile,
    type GitCommit,
    type GitCommitDetail,
  } from "./git";
  import { fullDate, shortSha, statusLetter, statusWord } from "./gitFormat";
  import { midTruncate } from "../previews/files";
  import { copyText } from "../shared/clipboard";
  import { referenceCommit } from "../shared/reference";
  import FileIcon from "../shared/FileIcon.svelte";
  import GitHistoryList from "./GitHistoryList.svelte";

  interface Props {
    wsId: string | null;
    paneId: string;
    ctrl: LayoutCtrl;
    tab: GitDetailTab;
  }
  let { wsId, paneId, ctrl, tab }: Props = $props();

  const repoArg = $derived(tab.repo ?? undefined);

  // A change in this repository (a commit in a terminal, an agent's write)
  // moves an epoch; the branch view and a history re-read then.
  const liveKey = $derived.by(() => {
    if (tab.repo !== null) {
      const st = $gitRepoStatuses.get(tab.repo);
      if (st !== undefined) return `r:${st.repo_epoch ?? 0}`;
    }
    return `p:${$gitStatus?.epoch ?? 0}`;
  });

  // ---- commit ------------------------------------------------------------
  let commit = $state<GitCommitDetail | null>(null);
  let branch = $state<GitBranchChanges | null>(null);
  let error = $state<string | null>(null);
  let slow = $state(false);
  let copied = $state(false);

  $effect(() => {
    const id = wsId;
    const view = tab.view;
    const sha = tab.sha;
    const base = tab.base;
    const repo = repoArg;
    if (id === null || view === "history") return;
    if (view === "branch") void liveKey;
    let live = true;
    error = null;
    const timer = setTimeout(() => {
      if (live) slow = true;
    }, 200);
    const done = (): void => {
      clearTimeout(timer);
      if (live) slow = false;
    };
    if (view === "commit" && sha !== undefined) {
      void fetchGitShow(id, sha, repo)
        .then((c) => {
          if (live) commit = c;
        })
        .catch((e: unknown) => {
          if (live) error = e instanceof Error ? e.message : "couldn't read the commit";
        })
        .finally(done);
    } else if (view === "branch") {
      void fetchGitCompare(id, repo, base)
        .then((b) => {
          if (live) branch = b;
        })
        .catch((e: unknown) => {
          if (live) error = e instanceof Error ? e.message : "couldn't compare the branch";
        })
        .finally(done);
    }
    return () => {
      live = false;
      clearTimeout(timer);
    };
  });

  /** A double-click keeps the tab a single click previews. */
  function keepOf(e: MouseEvent): { preview?: false } {
    return e.detail >= 2 ? { preview: false } : {};
  }

  function openFileDiff(e: MouseEvent, f: GitChangedFile): void {
    const split = e.metaKey || e.ctrlKey;
    if (tab.view === "commit" && commit !== null) {
      ctrl.openGitFrom(
        paneId,
        {
          surface: "diff",
          path: f.path,
          mode: "commit",
          rev: commit.sha,
          ...(tab.repo !== null ? { repo: tab.repo } : {}),
          ...(f.orig !== null ? { orig: f.orig } : {}),
          ...keepOf(e),
        },
        split,
      );
    } else if (tab.view === "branch" && branch !== null) {
      ctrl.openGitFrom(
        paneId,
        {
          surface: "diff",
          path: f.path,
          mode: "rev",
          rev: branch.diff_from,
          ...(tab.repo !== null ? { repo: tab.repo } : {}),
          ...keepOf(e),
        },
        split,
      );
    }
  }

  function openCommit(c: GitCommit, e: MouseEvent): void {
    ctrl.openGitFrom(
      paneId,
      { surface: "gitx", view: "commit", repo: tab.repo, sha: c.sha, title: c.subject, ...keepOf(e) },
      e.metaKey || e.ctrlKey,
    );
  }

  function dragCommit(c: GitCommit, e: PointerEvent): void {
    ctrl.beginGitDrag(
      e,
      { surface: "gitx", view: "commit", repo: tab.repo, sha: c.sha, title: c.subject },
      () => openCommit(c, e as unknown as MouseEvent),
    );
  }

  function copySha(): void {
    if (commit === null) return;
    void copyText(commit.sha).then(() => {
      copied = true;
      setTimeout(() => (copied = false), 1200);
    });
  }

  function referenceInChat(): void {
    if (commit === null) return;
    referenceCommit({ kind: "commit", sha: commit.sha, subject: commit.subject, repo: tab.repo });
  }

  /** A file's path in its repository (shown only when it has folders). */
  const relPath = $derived.by(() => {
    if (tab.path === undefined) return null;
    const r = repoForPath($gitRepos, tab.path);
    const rel = r !== null && tab.path.startsWith(`${r.path}/`) ? tab.path.slice(r.path.length + 1) : null;
    return rel !== null && rel.includes("/") ? rel : null;
  });

  const historyTitle = $derived(
    tab.path !== undefined
      ? `History of ${tab.path.split("/").pop() ?? tab.path}`
      : tab.rev !== undefined
        ? `History of ${tab.rev}`
        : "History",
  );
</script>

{#snippet counts(f: GitChangedFile)}
  {#if f.binary}
    <span class="fcount">binary</span>
  {:else if f.added !== null || f.removed !== null}
    <span class="fcount" title="lines added · removed"
      >+{f.added ?? 0} −{f.removed ?? 0}</span
    >
  {/if}
{/snippet}

{#snippet fileRow(f: GitChangedFile)}
  <button class="frow" title={`${f.path} — ${statusWord(f)}`} onclick={(e) => openFileDiff(e, f)}>
    <span class="ficon"><FileIcon path={f.path} size={13} /></span>
    <span class="fname">{midTruncate(f.rel, 64)}</span>
    {#if f.orig_rel}<span class="ffrom" title={`renamed from ${f.orig_rel}`}>←</span>{/if}
    <span class="spacer"></span>
    {@render counts(f)}
    <span class="fletter" title={statusWord(f)}>{statusLetter(f)}</span>
  </button>
{/snippet}

<div class="gx">
  {#if tab.view === "history"}
    <header class="head">
      <span class="title">{historyTitle}</span>
      {#if relPath !== null}<span class="sub" title={tab.path}>{relPath}</span>{/if}
    </header>
    <GitHistoryList
      {wsId}
      repo={tab.repo}
      path={tab.path}
      rev={tab.rev}
      initial={50}
      refreshKey={liveKey}
      onOpen={openCommit}
      onDragStart={dragCommit}
      fill
    />
  {:else if tab.view === "commit"}
    {#if commit !== null}
      <header class="head commit">
        <div class="subject">{commit.subject || "(no message)"}</div>
        {#if commit.body}
          <pre class="body">{commit.body}</pre>
        {/if}
        <div class="meta">
          <span>{commit.author}</span>
          <span class="dot">·</span>
          <span title={fullDate(commit.time)}>{fullDate(commit.time)}</span>
          <span class="dot">·</span>
          <button class="sha" title="copy the full sha" onclick={copySha}
            >{copied ? "copied" : shortSha(commit.sha)}</button
          >
          <span class="spacer"></span>
          <button class="action" title="reference this commit in the chat you're working with" onclick={referenceInChat}
            >Reference in chat</button
          >
        </div>
      </header>
      <div class="files">
        {#each commit.files as f (f.path)}
          {@render fileRow(f)}
        {/each}
        {#if commit.files.length === 0}
          <div class="note">No file changes.</div>
        {/if}
        {#if commit.truncated}
          <div class="note">More files than shown.</div>
        {/if}
      </div>
    {:else if error !== null}
      <div class="note">{error}</div>
    {:else if slow}
      <div class="note">Reading the commit…</div>
    {/if}
  {:else if tab.view === "branch"}
    {#if branch !== null}
      <header class="head">
        <span class="title">Changes on this branch</span>
        <span class="sub">
          {tab.title ?? ""}
          {#if branch.base !== null}
            {tab.title ? " · " : ""}since it left {branch.base}{#if branch.ahead !== null && branch.ahead > 0}
              · <button
                class="link"
                onclick={(e) =>
                  ctrl.openGitFrom(
                    paneId,
                    {
                      surface: "gitx",
                      view: "history",
                      repo: tab.repo,
                      ...(branch?.head ? { rev: branch.head } : {}),
                      title: tab.title,
                    },
                    e.metaKey || e.ctrlKey,
                  )}
                >{branch.ahead} commit{branch.ahead === 1 ? "" : "s"}</button
              >{/if}
          {:else}
            {tab.title ? " · " : ""}uncommitted work
          {/if}
        </span>
      </header>
      <div class="files">
        {#each branch.files as f (f.path + f.status)}
          {@render fileRow(f)}
        {/each}
        {#if branch.files.length === 0}
          <div class="note">Nothing yet.</div>
        {/if}
        {#if branch.truncated}
          <div class="note">More files than shown.</div>
        {/if}
      </div>
    {:else if error !== null}
      <div class="note">{error}</div>
    {:else if slow}
      <div class="note">Comparing…</div>
    {/if}
  {/if}
</div>

<style>
  .gx {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    background: var(--term-bg);
    overflow: hidden;
  }
  .head {
    flex: none;
    display: flex;
    align-items: baseline;
    flex-wrap: wrap;
    gap: 0.5rem;
    padding: 0.55rem 0.8rem 0.45rem;
    border-bottom: 1px solid var(--edge);
    min-width: 0;
  }
  .head.commit {
    display: block;
  }
  .title {
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--fg);
  }
  .sub {
    min-width: 0;
    font-size: var(--text-xs);
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .subject {
    font-size: var(--text-md);
    font-weight: 600;
    color: var(--fg);
    overflow-wrap: anywhere;
  }
  .body {
    margin: 0.4rem 0 0;
    max-height: 12rem;
    overflow: auto;
    font-family: inherit;
    font-size: var(--text-sm);
    line-height: 1.45;
    color: var(--muted);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .meta {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.15rem 0.35rem;
    margin-top: 0.45rem;
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .meta > * {
    white-space: nowrap;
  }
  .dot {
    opacity: 0.6;
  }
  .spacer {
    flex: 1;
  }
  .sha,
  .action,
  .link {
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    color: var(--muted);
    cursor: pointer;
  }
  .sha {
    font-family: var(--mono);
  }
  .sha:hover,
  .action:hover,
  .link:hover {
    color: var(--fg);
    text-decoration: underline;
    text-underline-offset: 2px;
  }
  .files {
    flex: 1;
    overflow-y: auto;
    padding: 0.3rem 0 0.6rem;
  }
  .frow {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    width: 100%;
    min-height: calc(var(--text-sm) + 9px);
    padding: 0 0.8rem;
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    text-align: left;
    cursor: pointer;
    color: var(--muted);
  }
  .frow:hover {
    background: var(--row-hover);
  }
  .frow:hover .fname {
    color: var(--fg);
  }
  .frow:focus-visible {
    outline: 1px solid var(--focus-ring);
    outline-offset: -1px;
  }
  .ficon {
    flex: none;
    display: flex;
  }
  .fname {
    min-width: 0;
    font-family: var(--mono);
    font-size: var(--text-sm);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .ffrom {
    flex: none;
    opacity: 0.6;
  }
  .fcount {
    flex: none;
    font-family: var(--mono);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    color: var(--muted);
  }
  .fletter {
    flex: none;
    width: 1ch;
    text-align: right;
    font-family: var(--mono);
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--muted);
  }
  .note {
    padding: 0.8rem;
    font-size: var(--text-sm);
    color: var(--muted);
  }
</style>
