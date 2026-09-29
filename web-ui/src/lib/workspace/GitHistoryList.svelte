<script lang="ts">
  /**
   * A history list: the latest `initial` commits, more as you scroll (pages
   * of 50) — a repository's, one file's, or a branch's. Calm loading: no
   * spinner for a quick answer, no layout jump when a page lands.
   */
  import { fetchGitLog, type GitCommit } from "./git";
  import CommitRow from "./CommitRow.svelte";

  interface Props {
    wsId: string | null;
    /** The repository (null = the workspace's own). */
    repo: string | null;
    path?: string;
    rev?: string;
    /** Rows shown before the first scroll. */
    initial?: number;
    /** Something moved (an epoch): the list reloads its first page. */
    refreshKey?: unknown;
    onOpen: (commit: GitCommit, e: MouseEvent) => void;
    onDragStart?: (commit: GitCommit, e: PointerEvent) => void;
    /** The list lives in its own scroller (a pane) rather than the panel's. */
    fill?: boolean;
  }
  let {
    wsId,
    repo,
    path,
    rev,
    initial = 20,
    refreshKey,
    onOpen,
    onDragStart,
    fill = false,
  }: Props = $props();

  const PAGE = 50;
  let commits = $state<GitCommit[]>([]);
  let hasMore = $state(false);
  let loading = $state(false);
  let slow = $state(false);
  let error = $state<string | null>(null);
  let unborn = $state(false);
  let seq = 0;

  async function loadFirst(): Promise<void> {
    if (wsId === null) return;
    const mine = ++seq;
    loading = true;
    error = null;
    const slowTimer = setTimeout(() => {
      if (mine === seq) slow = true;
    }, 200);
    try {
      const page = await fetchGitLog(wsId, {
        repo: repo ?? undefined,
        path,
        rev,
        limit: initial,
      });
      if (mine !== seq) return;
      commits = page.commits;
      hasMore = page.has_more;
      unborn = page.unborn === true;
    } catch (e) {
      if (mine !== seq) return;
      error = e instanceof Error ? e.message : "couldn't read the history";
    } finally {
      clearTimeout(slowTimer);
      if (mine === seq) {
        loading = false;
        slow = false;
      }
    }
  }

  async function loadMore(): Promise<void> {
    if (wsId === null || loading || !hasMore) return;
    const mine = seq;
    loading = true;
    try {
      const page = await fetchGitLog(wsId, {
        repo: repo ?? undefined,
        path,
        rev,
        skip: commits.length,
        limit: PAGE,
      });
      if (mine !== seq) return;
      commits = [...commits, ...page.commits];
      hasMore = page.has_more;
    } catch {
      // The next scroll tries again.
    } finally {
      if (mine === seq) loading = false;
    }
  }

  $effect(() => {
    void wsId;
    void repo;
    void path;
    void rev;
    void refreshKey;
    void loadFirst();
  });

  // More rows as the end scrolls into view.
  let sentinel = $state<HTMLElement | null>(null);
  $effect(() => {
    const el = sentinel;
    if (el === null || !hasMore) return;
    const io = new IntersectionObserver((entries) => {
      if (entries.some((e) => e.isIntersecting)) void loadMore();
    });
    io.observe(el);
    return () => io.disconnect();
  });
</script>

<div class="hist" class:fill>
  {#if error !== null && commits.length === 0}
    <div class="note">{error}</div>
  {:else if unborn}
    <div class="note">No commits yet.</div>
  {:else if commits.length === 0 && slow}
    <div class="note">Reading history…</div>
  {:else}
    {#each commits as c (c.sha)}
      <CommitRow
        commit={c}
        onOpen={(e) => onOpen(c, e)}
        onDragStart={onDragStart ? (e) => onDragStart(c, e) : undefined}
      />
    {/each}
    {#if hasMore}
      <div class="more" bind:this={sentinel} aria-hidden="true"></div>
    {/if}
  {/if}
</div>

<style>
  .hist {
    display: flex;
    flex-direction: column;
    min-height: 0;
  }
  .hist.fill {
    flex: 1;
    overflow-y: auto;
  }
  .note {
    padding: 0.5rem 0.8rem;
    font-size: var(--text-sm);
    color: var(--muted);
  }
  .more {
    height: 1px;
  }
</style>
