<script lang="ts">
  /**
   * A read-only look at files on the cluster: one `cluster_list` per folder
   * click (never a watcher, never a sync), and a file click copies that one
   * file to this machine and opens it in a preview window (`cluster_peek`).
   * Nothing is written on the cluster.
   */
  import { clusterList, clusterPeek, type ClusterListing } from "../net/native";
  import FileIcon from "../shared/FileIcon.svelte";
  import FolderIcon from "../shared/FolderIcon.svelte";
  import { humanSize } from "../previews/files";
  import { agoWords, childPath, parentPath, sortEntries } from "./cluster";

  interface Props {
    alias: string;
    /** Quick starting points (each workspace's folder). */
    places: { name: string; path: string }[];
  }

  let { alias, places }: Props = $props();

  let open = $state(false);
  let listing = $state<ClusterListing | null>(null);
  let pathInput = $state("");
  let loading = $state(false);
  let error = $state<string | null>(null);
  /** The file a peek is copying right now. */
  let peeking = $state<string | null>(null);
  let peekError = $state<string | null>(null);
  /** Drops an overtaken listing (a fast second click). */
  let seq = 0;

  const entries = $derived(listing === null ? [] : sortEntries(listing.entries));
  const up = $derived(listing === null ? null : parentPath(listing.path));

  async function go(path: string): Promise<void> {
    const target = path.trim();
    if (target === "") return;
    const mine = ++seq;
    loading = true;
    error = null;
    peekError = null;
    try {
      const next = await clusterList(alias, target);
      if (mine !== seq) return;
      listing = next;
      pathInput = next.path;
    } catch (e) {
      if (mine !== seq) return;
      error = e instanceof Error ? e.message : String(e);
    } finally {
      if (mine === seq) loading = false;
    }
  }

  async function peek(path: string): Promise<void> {
    if (peeking !== null) return;
    peeking = path;
    peekError = null;
    try {
      await clusterPeek(alias, path);
    } catch (e) {
      peekError = e instanceof Error ? e.message : String(e);
    } finally {
      peeking = null;
    }
  }

  function toggle(): void {
    open = !open;
    if (open && listing === null && !loading) {
      const start = places[0]?.path ?? "";
      if (start !== "") {
        pathInput = start;
        void go(start);
      }
    }
  }
</script>

<section class="files">
  <div class="sec-head">
    <button class="disclose" aria-expanded={open} onclick={toggle}>
      <svg class="chev" class:open viewBox="0 0 16 16" width="10" height="10" aria-hidden="true">
        <path d="M6 4l4 4-4 4" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" />
      </svg>
      <span class="sec-title">files</span>
    </button>
    {#if open}
      <span class="quiet-note">read-only · a file opens as a copy on this machine</span>
    {/if}
  </div>

  {#if open}
    {#if places.length > 1}
      <div class="places">
        {#each places as p, i (`${i}:${p.path}`)}
          <button class="place" title={p.path} onclick={() => void go(p.path)}>{p.name}</button>
        {/each}
      </div>
    {/if}
    <form
      class="bar"
      onsubmit={(e) => {
        e.preventDefault();
        void go(pathInput);
      }}
    >
      <input
        class="path-in"
        bind:value={pathInput}
        placeholder="a folder on {alias}, e.g. ~/ or $SCRATCH"
        spellcheck="false"
        autocomplete="off"
        aria-label="Folder on the cluster"
      />
      <button class="go" type="submit" disabled={loading || pathInput.trim() === ""}>
        {loading ? "Reading…" : "Go"}
      </button>
    </form>
    {#if error !== null}
      <div class="err-line">{error}</div>
    {/if}
    {#if peekError !== null}
      <div class="err-line">Couldn't open it: {peekError}</div>
    {/if}
    {#if listing !== null}
      <div class="list" class:stale={loading}>
        {#if up !== null}
          <button class="entry" onclick={() => up !== null && void go(up)}>
            <span class="icon up" aria-hidden="true">
              <svg viewBox="0 0 16 16" width="13" height="13">
                <path d="M8 13V3.5M4 7l4-4 4 4" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" />
              </svg>
            </span>
            <span class="ename muted">..</span>
          </button>
        {/if}
        {#each entries as e, i (`${i}:${e.name}`)}
          {@const full = childPath(listing.path, e.name)}
          <button
            class="entry"
            title={e.dir ? `Open ${full}` : `Open a copy of ${full}`}
            disabled={!e.dir && peeking !== null}
            onclick={() => (e.dir ? void go(full) : void peek(full))}
          >
            <span class="icon" aria-hidden="true">
              {#if e.dir}<FolderIcon size={14} />{:else}<FileIcon path={e.name} size={14} />{/if}
            </span>
            <span class="ename">{e.name}</span>
            {#if peeking === full}
              <span class="meta busy">opening…</span>
            {:else}
              <span class="meta size">{e.dir ? "" : humanSize(e.size)}</span>
              <span class="meta when">{e.mtime_ms > 0 ? agoWords(e.mtime_ms, Date.now()) : ""}</span>
            {/if}
          </button>
        {:else}
          <div class="empty">This folder is empty.</div>
        {/each}
      </div>
      {#if listing.truncated}
        <div class="quiet-note pad">Showing the first {listing.entries.length} entries.</div>
      {/if}
    {:else if !loading && error === null}
      <p class="hint">Type a folder, or pick a workspace above.</p>
    {/if}
  {/if}
</section>

<style>
  .files {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .sec-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    padding: 0 8px 4px;
  }

  .disclose {
    appearance: none;
    border: none;
    background: none;
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 0;
    color: var(--muted);
    cursor: pointer;
    font: inherit;
  }

  .disclose:hover .sec-title {
    color: var(--fg);
  }

  .chev {
    flex: none;
    transition: transform 0.12s ease;
  }

  .chev.open {
    transform: rotate(90deg);
  }

  .sec-title {
    font-size: var(--text-xs);
    color: var(--muted);
    text-transform: lowercase;
    letter-spacing: 0.04em;
  }

  .quiet-note {
    font-size: var(--text-xs);
    color: var(--muted);
    opacity: 0.85;
  }

  .quiet-note.pad {
    padding: 2px 10px;
  }

  .places {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    padding: 0 10px;
  }

  .place {
    appearance: none;
    font: inherit;
    font-family: var(--mono);
    font-size: var(--text-xs);
    padding: 2px 9px;
    border-radius: 999px;
    border: 1px solid var(--edge);
    background: none;
    color: var(--muted);
    cursor: pointer;
  }

  .place:hover {
    color: var(--fg);
    border-color: color-mix(in srgb, var(--accent) 50%, var(--edge));
  }

  .bar {
    display: flex;
    gap: 8px;
    padding: 0 10px;
  }

  .path-in {
    flex: 1;
    min-width: 0;
    border: 1px solid var(--edge);
    border-radius: 6px;
    background: var(--overlay-bg);
    color: var(--fg);
    font-family: var(--mono);
    font-size: var(--text-sm);
    padding: 5px 10px;
    outline: none;
  }

  .path-in:focus {
    border-color: var(--focus-ring);
  }

  .path-in::placeholder {
    color: var(--muted);
    opacity: 0.7;
  }

  .go {
    appearance: none;
    border: 1px solid var(--edge);
    background: var(--overlay-bg);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
    padding: 4px 12px;
    border-radius: 6px;
    cursor: pointer;
  }

  .go:hover:enabled {
    border-color: var(--accent);
  }

  .go:disabled {
    opacity: 0.55;
    cursor: default;
  }

  .list {
    display: flex;
    flex-direction: column;
    max-height: 360px;
    overflow-y: auto;
    margin: 0 10px;
    border: 1px solid var(--edge);
    border-radius: 7px;
    scrollbar-width: thin;
    transition: opacity 0.12s ease;
  }

  .list.stale {
    opacity: 0.6;
  }

  .entry {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    color: var(--fg);
    text-align: left;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 5px 10px;
    cursor: pointer;
  }

  .entry:hover:enabled {
    background: var(--row-hover);
  }

  .entry:disabled {
    cursor: progress;
  }

  .icon {
    flex: none;
    display: inline-flex;
    width: 14px;
    justify-content: center;
    color: var(--muted);
  }

  .ename {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono);
    font-size: var(--text-sm);
  }

  .ename.muted {
    color: var(--muted);
  }

  .meta {
    flex: none;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
  }

  .meta.size {
    width: 64px;
    text-align: right;
  }

  .meta.when {
    width: 82px;
    text-align: right;
    opacity: 0.8;
  }

  .meta.busy {
    color: var(--accent);
  }

  .empty {
    padding: 10px;
    font-size: var(--text-sm);
    color: var(--muted);
  }

  .hint {
    margin: 0;
    padding: 2px 10px;
    font-size: var(--text-sm);
    color: var(--muted);
  }

  .err-line {
    padding: 0 10px;
    font-size: var(--text-sm);
    color: var(--err);
    white-space: pre-wrap;
  }
</style>
