<script lang="ts">
  import { onMount } from "svelte";
  import { ApiError, getToken } from "./api";
  import {
    createWorkspace,
    fsDirs,
    fsHome,
    type DirEntry,
    type DirListing,
    type Workspace,
  } from "./sessions";

  interface Props {
    /** Known workspaces, shown as "recent" until the user types or navigates. */
    recents: Workspace[];
    /** A workspace was opened in THIS window. */
    onOpened: (w: Workspace) => void;
    onClose: () => void;
  }

  let { recents, onOpened, onClose }: Props = $props();

  type Row =
    | { kind: "recent"; ws: Workspace }
    | { kind: "here"; path: string }
    | { kind: "dir"; dir: DirEntry };

  let input = $state("");
  let listing = $state<DirListing | null>(null);
  let error = $state<string | null>(null);
  let highlight = $state(0);
  let navigated = $state(false);
  let listEl = $state<HTMLDivElement | null>(null);
  let crumbsEl = $state<HTMLDivElement | null>(null);
  let busy = false;

  const showRecents = $derived(!navigated && input === "" && recents.length > 0);
  const filtered = $derived(
    listing === null
      ? []
      : listing.dirs.filter((d) => d.name.toLowerCase().includes(input.trim().toLowerCase())),
  );
  const rows = $derived.by((): Row[] => {
    const out: Row[] = [];
    if (showRecents) {
      for (const ws of recents) out.push({ kind: "recent", ws });
    }
    if (listing !== null) {
      out.push({ kind: "here", path: listing.path });
      for (const dir of filtered) out.push({ kind: "dir", dir });
    }
    return out;
  });
  const browseOffset = $derived(showRecents ? recents.length : 0);
  const crumbs = $derived.by((): { name: string; path: string }[] => {
    if (listing === null) return [];
    const out: { name: string; path: string }[] = [];
    let acc = "";
    for (const part of listing.path.split("/")) {
      if (part === "") continue;
      acc += `/${part}`;
      out.push({ name: part, path: acc });
    }
    return out;
  });

  function rowPath(row: Row | undefined): string | null {
    if (row === undefined) return listing?.path ?? null;
    switch (row.kind) {
      case "recent":
        return row.ws.root;
      case "here":
        return row.path;
      case "dir":
        return row.dir.path;
    }
  }

  function resetHighlight(): void {
    if (showRecents) {
      highlight = 0;
      return;
    }
    // Prefer the first subdirectory (Enter descends); fall back to
    // "open this folder" when nothing matches.
    const firstDir = rows.findIndex((r) => r.kind === "dir");
    highlight = firstDir >= 0 ? firstDir : 0;
  }

  async function browse(path: string): Promise<boolean> {
    try {
      listing = await fsDirs(path);
      error = null;
      return true;
    } catch (e) {
      error = e instanceof ApiError ? e.message : "could not read directory";
      return false;
    }
  }

  async function navigate(path: string): Promise<void> {
    if (await browse(path)) {
      navigated = true;
      input = "";
      resetHighlight();
    }
  }

  async function openHere(path: string | null): Promise<void> {
    if (path === null || busy) return;
    busy = true;
    try {
      onOpened(await createWorkspace(path));
    } catch (e) {
      error = e instanceof ApiError ? e.message : "could not open folder";
    } finally {
      busy = false;
    }
  }

  async function openNewWindow(path: string | null): Promise<void> {
    if (path === null || busy) return;
    busy = true;
    try {
      const w = await createWorkspace(path);
      const token = getToken();
      const hash =
        token !== null
          ? `#token=${encodeURIComponent(token)}&ws=${encodeURIComponent(w.id)}`
          : `#ws=${encodeURIComponent(w.id)}`;
      window.open(`${location.origin}/${hash}`);
      onClose();
    } catch (e) {
      error = e instanceof ApiError ? e.message : "could not open folder";
    } finally {
      busy = false;
    }
  }

  function onKeydown(e: KeyboardEvent): void {
    if (e.key === "Escape") {
      e.preventDefault();
      onClose();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      if (rows.length > 0) highlight = Math.min(highlight + 1, rows.length - 1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      if (rows.length > 0) highlight = Math.max(highlight - 1, 0);
    } else if (e.key === "Backspace" && input === "") {
      e.preventDefault();
      const parent = listing?.parent;
      if (parent) void navigate(parent);
    } else if (e.key === "Enter") {
      e.preventDefault();
      if (e.metaKey || e.ctrlKey) {
        void openHere(rowPath(rows[highlight]));
        return;
      }
      const typed = input.trim();
      if (typed.startsWith("/") || typed.startsWith("~")) {
        void navigate(typed);
        return;
      }
      const row = rows[highlight];
      if (row === undefined) return;
      if (row.kind === "dir") {
        void navigate(row.dir.path);
      } else {
        void openHere(rowPath(row));
      }
    }
  }

  /** Keep the filter input focused when clicking rows/crumbs. */
  function keepFocus(e: MouseEvent): void {
    e.preventDefault();
  }

  function focusOnMount(node: HTMLElement): void {
    node.focus();
  }

  onMount(() => {
    void fsHome()
      .then(async (home) => {
        await browse(home);
        resetHighlight();
      })
      .catch((e: unknown) => {
        error = e instanceof ApiError ? e.message : "could not reach the daemon";
      });
  });

  // Keep the highlighted row in view and the breadcrumb pinned to its tail.
  $effect(() => {
    const el = listEl?.querySelector(`[data-idx="${highlight}"]`);
    el?.scrollIntoView({ block: "nearest" });
  });
  $effect(() => {
    void listing?.path;
    const el = crumbsEl;
    if (el) el.scrollLeft = el.scrollWidth;
  });
</script>

{#snippet chevron()}
  <svg class="chev" viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
    <path
      d="M6 4l4 4-4 4"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      stroke-linejoin="round"
    />
  </svg>
{/snippet}

{#snippet newWindow(path: string)}
  <button
    class="side"
    tabindex="-1"
    onmousedown={keepFocus}
    onclick={(e) => {
      e.stopPropagation();
      void openNewWindow(path);
    }}>new window</button
  >
{/snippet}

<div class="overlay">
  <button class="scrim" aria-label="close" tabindex="-1" onclick={onClose}></button>
  <div class="panel" role="dialog" aria-modal="true" aria-label="open folder">
    <input
      class="filter"
      bind:value={input}
      placeholder="filter, or type a path"
      spellcheck="false"
      autocomplete="off"
      use:focusOnMount
      onkeydown={onKeydown}
      oninput={resetHighlight}
    />
    <div class="crumbs" bind:this={crumbsEl}>
      {#if listing !== null}
        {#if crumbs.length === 0}
          <span class="sep">/</span>
        {:else}
          {#each crumbs as c (c.path)}
            <span class="sep">/</span>
            <button class="crumb" tabindex="-1" onmousedown={keepFocus} onclick={() => void navigate(c.path)}
              >{c.name}</button
            >
          {/each}
        {/if}
      {/if}
    </div>
    <div class="list" bind:this={listEl}>
      {#if error !== null}
        <div class="error">{error}</div>
      {/if}
      {#if showRecents}
        <div class="section">recent</div>
        {#each recents as ws, i (ws.id)}
          <div class="rowwrap" class:hl={highlight === i} data-idx={i}>
            <button
              class="row"
              tabindex="-1"
              title={ws.root}
              onmousedown={keepFocus}
              onclick={() => void openHere(ws.root)}
            >
              <span class="name">{ws.name}</span>
            </button>
            {@render newWindow(ws.root)}
          </div>
        {/each}
        <div class="section">browse</div>
      {/if}
      {#if listing !== null}
        <div class="rowwrap" class:hl={highlight === browseOffset} data-idx={browseOffset}>
          <button
            class="row"
            tabindex="-1"
            title={listing.path}
            onmousedown={keepFocus}
            onclick={() => void openHere(listing?.path ?? null)}
          >
            <span class="name">open this folder</span>
          </button>
          {@render newWindow(listing.path)}
        </div>
        {#each filtered as dir, j (dir.path)}
          {@const idx = browseOffset + 1 + j}
          <div class="rowwrap" class:hl={highlight === idx} data-idx={idx}>
            <button
              class="row"
              tabindex="-1"
              title={dir.path}
              onmousedown={keepFocus}
              onclick={() => void navigate(dir.path)}
            >
              <span class="name">{dir.name}</span>
              {@render chevron()}
            </button>
            {@render newWindow(dir.path)}
          </div>
        {/each}
      {/if}
    </div>
  </div>
</div>

<style>
  .overlay {
    position: fixed;
    inset: 0;
    z-index: 100;
    animation: fade 0.1s ease-out;
  }

  @keyframes fade {
    from {
      opacity: 0;
    }
  }

  .scrim {
    position: absolute;
    inset: 0;
    appearance: none;
    border: none;
    padding: 0;
    background: var(--scrim);
    cursor: default;
  }

  .panel {
    position: relative;
    width: min(560px, calc(100vw - 2rem));
    max-height: 60vh;
    margin: 13vh auto 0;
    display: flex;
    flex-direction: column;
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 8px;
    box-shadow: 0 12px 36px rgba(0, 0, 0, 0.22);
    overflow: hidden;
  }

  .filter {
    flex: none;
    border: none;
    outline: none;
    background: none;
    color: var(--fg);
    font: inherit;
    font-size: 0.9rem;
    padding: 0.7rem 0.9rem 0.4rem;
  }

  .filter::placeholder {
    color: var(--muted);
    opacity: 0.7;
  }

  .crumbs {
    flex: none;
    display: flex;
    align-items: center;
    white-space: nowrap;
    overflow-x: auto;
    scrollbar-width: none;
    padding: 0 0.9rem 0.5rem;
    border-bottom: 1px solid var(--edge);
    font-family: var(--mono);
    font-size: 0.72rem;
    color: var(--muted);
    min-height: 1.4em;
  }

  .crumbs::-webkit-scrollbar {
    display: none;
  }

  .sep {
    opacity: 0.55;
  }

  .crumb {
    appearance: none;
    border: none;
    background: none;
    padding: 0 0.1rem;
    font: inherit;
    color: inherit;
    cursor: pointer;
  }

  .crumb:hover {
    color: var(--fg);
  }

  .list {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 0.3rem 0.45rem 0.45rem;
  }

  .section {
    padding: 0.45rem 0.45rem 0.15rem;
    font-size: 0.75rem;
    color: var(--muted);
  }

  .error {
    padding: 0.4rem 0.45rem;
    font-size: 0.75rem;
    color: var(--err);
  }

  .rowwrap {
    display: flex;
    align-items: center;
    border-radius: 5px;
  }

  .rowwrap:hover {
    background: var(--row-hover);
  }

  .rowwrap.hl {
    background: var(--row-active);
  }

  .row {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 0.5rem;
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: 0.85rem;
    color: var(--fg);
    text-align: left;
    padding: 0.32rem 0.45rem;
    cursor: pointer;
  }

  .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono);
    font-size: 0.78rem;
  }

  .chev {
    flex: none;
    color: var(--muted);
    opacity: 0.6;
  }

  .side {
    flex: none;
    display: none;
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: 0.72rem;
    color: var(--muted);
    padding: 0.2rem 0.45rem;
    cursor: pointer;
  }

  .side:hover {
    color: var(--fg);
  }

  .rowwrap:hover .side,
  .rowwrap.hl .side {
    display: block;
  }
</style>
