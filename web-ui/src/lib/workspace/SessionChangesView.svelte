<script lang="ts">
  import {
    fetchGitRepos,
    fetchGitStatus,
    fetchSessionGit,
    gitEnv,
    gitRepos,
    gitRepoStatuses,
    gitStatus,
    openGitView,
    repoForPath,
    type DiffMode,
    type GitEntry,
    type GitRepo,
    type GitStatus,
    type SessionGitStory,
  } from "./git";
  import CommitRow from "./CommitRow.svelte";
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

  /** Several repositories in one workspace: a touched file takes the status
   *  of the innermost repository holding it. The active workspace's list and
   *  statuses come from the git store; another workspace's are fetched here.
   *  At most 8 nested repositories are asked, only those holding a touched
   *  file. */
  const REPOS_ASKED_MAX = 8;
  const sameWs = $derived(
    activeStatus !== null && activeStatus.workspace_id === session.workspace_id,
  );
  let ownRepos = $state<GitRepo[]>([]);
  $effect(() => {
    const wsId = session.workspace_id;
    if (sameWs) {
      ownRepos = [];
      return;
    }
    let cancelled = false;
    void fetchGitRepos(wsId).then(
      (list) => {
        if (!cancelled) ownRepos = list.repos ?? [];
      },
      () => {
        if (!cancelled) ownRepos = [];
      },
    );
    return () => {
      cancelled = true;
    };
  });
  const repos = $derived(sameWs ? $gitRepos : ownRepos);
  /** The nested repositories (below the root) holding a touched file. */
  const nestedWithFiles = $derived.by(() => {
    const out = new Set<string>();
    for (const p of session.files_touched ?? []) {
      const r = repoForPath(repos, p);
      if (r !== null && (r.kind === "nested" || r.kind === "submodule")) out.add(r.path);
      if (out.size >= REPOS_ASKED_MAX) break;
    }
    return [...out];
  });
  let fetchedStatuses = $state<Map<string, GitStatus>>(new Map());
  $effect(() => {
    const wsId = session.workspace_id;
    void session.files_touched?.length;
    void status?.repo_epoch;
    const wanted = nestedWithFiles.filter((top) => !(sameWs && $gitRepoStatuses.has(top)));
    if (wanted.length === 0) return;
    let cancelled = false;
    void Promise.all(
      wanted.map((top) =>
        fetchGitStatus(wsId, top).then(
          (st) => [top, st] as const,
          () => null,
        ),
      ),
    ).then((pairs) => {
      if (cancelled) return;
      const next = new Map<string, GitStatus>();
      for (const pair of pairs) if (pair !== null && pair[1].repo) next.set(pair[0], pair[1]);
      fetchedStatuses = next;
    });
    return () => {
      cancelled = true;
    };
  });

  /** Absolute path -> its git entry, for the touched files: the primary
   *  repository's entries, then each nested one's (innermost wins). */
  const byPath = $derived.by(() => {
    const m = new Map<string, GitEntry>();
    for (const e of status?.entries ?? []) m.set(e.path, e);
    for (const top of nestedWithFiles) {
      const st = (sameWs ? $gitRepoStatuses.get(top) : undefined) ?? fetchedStatuses.get(top);
      for (const e of st?.entries ?? []) m.set(e.path, e);
    }
    return m;
  });
  const inRepo = $derived(status !== null || nestedWithFiles.length > 0);

  /** The commits this session made (its git story), refetched as it writes
   *  and as its repository moves. Nothing is asked outside a repository. */
  let story = $state<SessionGitStory | null>(null);
  $effect(() => {
    const id = session.id;
    void session.files_touched?.length;
    void status?.repo_epoch;
    const branch = session.git?.branch;
    if (session.git == null) {
      story = null;
      return;
    }
    void branch;
    let cancelled = false;
    void fetchSessionGit(id).then(
      (g) => {
        if (!cancelled) story = g;
      },
      () => {
        if (!cancelled) story = null;
      },
    );
    return () => {
      cancelled = true;
    };
  });
  const commits = $derived(story?.commits ?? []);
  function openCommit(c: { sha: string; subject: string }): void {
    const repo = story?.current?.worktree ?? story?.start?.worktree ?? null;
    openGitView({ surface: "gitx", view: "commit", repo, sha: c.sha, title: c.subject });
  }

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

  {#if commits.length > 0}
    <!-- The commits this session made lead, rows like History's. -->
    <section class="commits" aria-label="Commits this session made">
      <div class="sub">
        Committed {commits.length}{story?.truncated ? "+" : ""}{#if story?.rewritten}<span
            class="muted"
          >
            · history was rewritten since it started</span
          >{/if}
      </div>
      {#each commits as c (c.sha)}
        <CommitRow
          commit={{ sha: c.sha, parents: [], author: "", time: c.time, subject: c.subject }}
          onOpen={() => openCommit(c)}
        />
      {/each}
    </section>
  {/if}

  <!-- The files, each with its edit count: a click opens the git diff when
       the file has an uncommitted change, else the agent's own edits for it,
       in order — one list, with or without git. -->
  <div class="list">
    <SessionEdits
      sessionId={session.id}
      wsId={session.workspace_id}
      wsRoot={base}
      paths={files}
      repo={inRepo}
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
  .commits {
    padding: 6px 8px 2px;
    border-bottom: 1px solid var(--edge);
  }
  .sub {
    padding: 2px 6px 4px;
    color: var(--muted);
    font-size: var(--text-sm);
  }
  .muted {
    color: var(--muted);
  }
</style>
