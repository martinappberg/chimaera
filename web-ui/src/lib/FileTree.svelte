<script lang="ts">
  /**
   * Lazy directory tree for the rail's FILES section. Each expanded dir is
   * one /fs/list call; listings are cached per path and refreshed every time
   * the dir is re-expanded. The tree renders flat rows (indent by depth) —
   * no recursive components, trivial scrolling.
   */
  import { tick, untrack } from "svelte";
  import { fsList, type FsEntry } from "./files";
  import FileIcon from "./FileIcon.svelte";

  interface Props {
    /** Workspace root on the daemon's filesystem. */
    root: string;
    /** Open a file surface in the layout. */
    onOpen(path: string): void;
    /** Begin a pointer drag of a file entry (same grammar as rail rows and
     *  pane tabs; a sub-threshold release becomes a plain open). */
    onDragStart(e: PointerEvent, path: string): void;
    /** The focused pane's active file, for the subtle current marker. */
    activePath: string | null;
    /**
     * Reveal request (terminal dir links): expand the ancestor chain of
     * `path` and scroll it into view. The nonce distinguishes repeats.
     */
    reveal?: { path: string; nonce: number } | null;
  }

  let { root, onOpen, onDragStart, activePath, reveal = null }: Props = $props();

  let expanded = $state<Set<string>>(new Set());
  let listings = $state<Map<string, FsEntry[]>>(new Map());
  let loading = $state<Set<string>>(new Set());
  let rootError = $state<string | null>(null);

  interface Row {
    entry: FsEntry;
    depth: number;
  }

  // Quiet client-side filter over the LOADED tree: narrows visible entries by
  // a case-insensitive name match, keeping the ancestor dirs of any match so
  // the structure stays legible. Revealed by the affordance or by typing while
  // the tree is focused.
  let filter = $state("");
  let filterOpen = $state(false);
  let filterEl = $state<HTMLInputElement | null>(null);
  const filterQuery = $derived(filter.trim().toLowerCase());

  const rows = $derived.by(() => {
    const q = filterQuery;
    const out: Row[] = [];
    // Returns true when this subtree contributed at least one visible row.
    const walk = (dir: string, depth: number): boolean => {
      const entries = listings.get(dir);
      if (entries === undefined) return false;
      let any = false;
      for (const e of entries) {
        const selfMatch = q === "" || e.name.toLowerCase().includes(q);
        if (e.kind === "dir") {
          // A filtered dir is shown when it (or a loaded descendant) matches;
          // expand into it while filtering even if collapsed, so matches surface.
          const descend = q !== "" || expanded.has(e.path);
          const marker: Row = { entry: e, depth };
          const before = out.length;
          out.push(marker);
          const childMatched = descend ? walk(e.path, depth + 1) : false;
          if (q !== "" && !selfMatch && !childMatched) {
            out.length = before; // prune a dir with no matches under it
          } else {
            any = true;
          }
        } else if (selfMatch) {
          out.push({ entry: e, depth });
          any = true;
        }
      }
      return any;
    };
    walk(root, 0);
    return out;
  });

  function openFilter(seed = ""): void {
    filterOpen = true;
    if (seed !== "") filter = seed;
    void Promise.resolve().then(() => filterEl?.focus());
  }

  function closeFilter(): void {
    filter = "";
    filterOpen = false;
  }

  /** Typing a printable character with the tree focused opens the filter. */
  function onTreeKeydown(e: KeyboardEvent): void {
    if (filterOpen || e.metaKey || e.ctrlKey || e.altKey) return;
    if (e.key.length === 1 && e.key !== " ") {
      openFilter(e.key);
      e.preventDefault();
    }
  }

  function onFilterKeydown(e: KeyboardEvent): void {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      closeFilter();
    } else if (e.key === "Enter") {
      e.preventDefault();
      // Enter opens the first matching file (skip dirs), a fast keyboard path.
      const hit = rows.find((r) => r.entry.kind === "file");
      if (hit !== undefined) onOpen(hit.entry.path);
    }
  }

  // Load (or reload) the root whenever the workspace changes. The body
  // writes the state it also reads (via load), so it must not track it —
  // only `root` is a dependency.
  $effect(() => {
    const r = root;
    untrack(() => {
      expanded = new Set();
      listings = new Map();
      rootError = null;
      void load(r);
    });
  });

  async function load(dir: string): Promise<void> {
    loading = new Set(loading).add(dir);
    try {
      const listing = await fsList(dir);
      const next = new Map(listings);
      next.set(dir, listing.entries);
      listings = next;
      if (dir === root) rootError = null;
    } catch (e) {
      if (dir === root) {
        rootError = e instanceof Error ? e.message : "failed to list files";
      } else {
        // Collapse a dir that failed to list (deleted, permission denied).
        const n = new Set(expanded);
        n.delete(dir);
        expanded = n;
      }
    } finally {
      const n = new Set(loading);
      n.delete(dir);
      loading = n;
    }
  }

  /** Row briefly highlighted after a reveal (fades on its own). */
  let flashPath = $state<string | null>(null);
  let treeEl = $state<HTMLElement | null>(null);

  // Reveal requests (terminal dir links, touched files): expand the ancestor
  // chain, refresh its listings (the target may be brand new), scroll the
  // row into view, and flash it.
  $effect(() => {
    const req = reveal;
    if (req == null) return;
    untrack(() => void doReveal(req.path));
  });

  async function doReveal(path: string): Promise<void> {
    const r = root.endsWith("/") && root.length > 1 ? root.slice(0, -1) : root;
    if (path !== r && !path.startsWith(`${r}/`)) return;
    closeFilter();
    const rel = path === r ? "" : path.slice(r.length + 1);
    const parts = rel === "" ? [] : rel.split("/");
    // Expand every ancestor; the target itself expands too when it is a dir
    // (its row exists either way — the parent listing decides its kind).
    const chain: string[] = [];
    let cur = r;
    for (const part of parts) {
      cur = `${cur}/${part}`;
      chain.push(cur);
    }
    for (const d of [r, ...chain.slice(0, -1)]) {
      await load(d); // fresh listings — the path may have just been created
    }
    const target = chain.at(-1) ?? r;
    const isDir = listings.get(parentOf(target))?.some((e) => e.path === target && e.kind === "dir");
    const next = new Set(expanded);
    for (const d of chain.slice(0, -1)) next.add(d);
    if (isDir === true) {
      next.add(target);
      expanded = next;
      await load(target);
    } else {
      expanded = next;
    }
    flashPath = path;
    await tick();
    treeEl
      ?.querySelector(`[data-path="${CSS.escape(path)}"]`)
      ?.scrollIntoView({ block: "nearest" });
    setTimeout(() => {
      if (flashPath === path) flashPath = null;
    }, 1200);
  }

  function parentOf(path: string): string {
    const i = path.lastIndexOf("/");
    return i > 0 ? path.slice(0, i) : "/";
  }

  function toggle(dir: string): void {
    const next = new Set(expanded);
    if (next.has(dir)) {
      next.delete(dir);
      expanded = next;
    } else {
      next.add(dir);
      expanded = next;
      void load(dir); // fresh listing on every expand
    }
  }

  function onRowKey(e: KeyboardEvent, entry: FsEntry): void {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      if (entry.kind === "dir") toggle(entry.path);
      else onOpen(entry.path);
    }
  }
</script>

<div class="tree-wrap">
  <div class="filter-bar" class:open={filterOpen}>
    {#if filterOpen}
      <svg class="filter-icon" viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
        <circle cx="7" cy="7" r="4" fill="none" stroke="currentColor" stroke-width="1.4" />
        <line x1="10" y1="10" x2="13.5" y2="13.5" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" />
      </svg>
      <input
        class="filter-input"
        bind:this={filterEl}
        bind:value={filter}
        placeholder="filter files"
        spellcheck="false"
        autocomplete="off"
        aria-label="filter files"
        onkeydown={onFilterKeydown}
      />
      <button class="filter-clear" aria-label="clear filter" title="clear filter" onclick={closeFilter}>&times;</button>
    {:else}
      <button class="filter-toggle" aria-label="filter files" title="filter files" onclick={() => openFilter()}>
        <svg viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
          <circle cx="7" cy="7" r="4" fill="none" stroke="currentColor" stroke-width="1.4" />
          <line x1="10" y1="10" x2="13.5" y2="13.5" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" />
        </svg>
      </button>
    {/if}
  </div>
  <div
    class="tree"
    role="tree"
    tabindex="-1"
    bind:this={treeEl}
    onkeydown={onTreeKeydown}
  >
  {#if rootError !== null}
    <div class="tree-error">{rootError}</div>
  {:else if listings.get(root)?.length === 0}
    <div class="tree-empty">empty</div>
  {:else if filterQuery !== "" && rows.length === 0}
    <div class="tree-empty">no matches</div>
  {/if}
  {#each rows as { entry, depth } (entry.path)}
    <div
      class="node"
      class:active={entry.path === activePath}
      class:flash={entry.path === flashPath}
      role="treeitem"
      aria-expanded={entry.kind === "dir" ? expanded.has(entry.path) : undefined}
      aria-selected={entry.path === activePath}
      tabindex="0"
      title={entry.path}
      data-path={entry.path}
      style:padding-left={`${8 + depth * 13}px`}
      onclick={() => {
        // Files open via the drag's sub-threshold click path (below), so a
        // completed drag never ALSO opens the file in the focused pane.
        if (entry.kind === "dir") toggle(entry.path);
      }}
      onpointerdowncapture={(e) => {
        if (entry.kind === "file") onDragStart(e, entry.path);
      }}
      onkeydown={(e) => onRowKey(e, entry)}
    >
      {#if entry.kind === "dir"}
        <svg
          class="chev"
          class:open={expanded.has(entry.path)}
          class:busy={loading.has(entry.path)}
          viewBox="0 0 16 16"
          width="9"
          height="9"
          aria-hidden="true"
        >
          <path
            d="M6 4l4 4-4 4"
            fill="none"
            stroke="currentColor"
            stroke-width="1.6"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
        </svg>
      {:else}
        <span class="file-glyph"><FileIcon path={entry.path} size={14} /></span>
      {/if}
      <span class="node-name" class:dir={entry.kind === "dir"}>{entry.name}</span>
    </div>
  {/each}
  </div>
</div>

<style>
  .tree-wrap {
    display: flex;
    flex-direction: column;
    min-height: 0;
  }

  /* Quiet filter affordance: a small magnifier that expands into an input.
     The collapsed toggle sits flush-right so it never competes with the tree. */
  .filter-bar {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: flex-end;
    padding: 0 0.55rem 2px;
    min-height: 20px;
  }

  .filter-bar.open {
    justify-content: stretch;
    gap: 0.3rem;
  }

  .filter-toggle,
  .filter-clear {
    appearance: none;
    border: none;
    background: none;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 2px;
    border-radius: 4px;
    color: var(--muted);
    cursor: pointer;
    opacity: 0.7;
    transition:
      opacity 0.12s ease,
      color 0.12s ease,
      background-color 0.12s ease;
  }

  .filter-toggle:hover,
  .filter-clear:hover {
    opacity: 1;
    color: var(--fg);
    background: var(--row-hover);
  }

  .filter-icon {
    flex: none;
    color: var(--muted);
    opacity: 0.7;
  }

  .filter-clear {
    font-size: var(--text-md);
    line-height: 1;
    padding: 0 0.2rem;
  }

  .filter-input {
    flex: 1;
    min-width: 0;
    border: none;
    outline: none;
    background: none;
    font-family: var(--mono);
    font-size: 0.74rem;
    color: var(--fg);
    padding: 1px 0;
  }

  .filter-input::placeholder {
    color: var(--muted);
    opacity: 0.7;
  }

  .tree {
    display: flex;
    flex-direction: column;
    padding: 2px 0.45rem 0.5rem;
    outline: none;
  }

  .tree-error,
  .tree-empty {
    padding: 0.3rem 0.45rem;
    font-size: 0.72rem;
    color: var(--muted);
    line-height: 1.4;
  }

  .node {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    height: 22px;
    padding-right: 0.45rem;
    border-radius: 4px;
    cursor: pointer;
    user-select: none;
    min-width: 0;
    outline: none;
  }

  .node:hover {
    background: var(--row-hover);
  }

  .node:focus-visible {
    background: var(--row-hover);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--accent) 45%, transparent);
  }

  .node.active {
    background: var(--row-active);
  }

  /* Reveal flash (terminal dir links): a brief accent wash that fades. */
  .node.flash {
    background: color-mix(in srgb, var(--accent) 18%, transparent);
    transition: background-color 0.9s ease;
  }

  .chev {
    flex: none;
    color: var(--muted);
    opacity: 0.8;
    transition: transform 0.1s ease;
  }

  .chev.open {
    transform: rotate(90deg);
  }

  .chev.busy {
    opacity: 0.4;
  }

  /* File glyph sits in the chevron column; a hair inset so its 14px body
     lines up with the 9px dir chevrons above it. */
  .file-glyph {
    flex: none;
    display: flex;
    align-items: center;
    margin: 0 -2px 0 -3px;
  }

  .node-name {
    font-family: var(--mono);
    font-size: 0.74rem;
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }

  .node-name.dir {
    color: var(--fg);
  }

  .node.active .node-name {
    color: var(--fg);
  }
</style>
