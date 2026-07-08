<script lang="ts">
  /**
   * The Finder: a macOS-style Miller-columns file browser hosted as a pane
   * surface. Each directory in the current path is one column (one /fs/list
   * call); descending appends a column to the right, so the whole chain stays
   * visible. It browses anywhere the daemon can reach — inside or outside the
   * workspace — with the full absolute path always shown in the breadcrumb, and
   * a quiet marker when the location sits outside the workspace root.
   *
   * Navigation state lives in the layout tab (the `path` prop): internal
   * navigation reports up via `onNavigate` (persisted), and an external change
   * to `path` (a terminal dir-link redirecting this Finder) is reconciled by
   * the effect below. Opening a file hands off to the workbench via
   * `onOpenFile` — every file viewer is reused unchanged.
   */
  import { untrack } from "svelte";
  import { fsList, type FsEntry, humanSize } from "./files";
  import { fsHome } from "./sessions";
  import { getSetting } from "./settings/store.svelte";
  import { ApiError } from "./api";
  import FileIcon from "./FileIcon.svelte";
  import FolderIcon from "./FolderIcon.svelte";

  interface Props {
    /** The Finder's current directory (seed + external-nav channel). */
    path: string;
    /** Workspace root, for the "inside/outside workspace" marker + root jump. */
    wsRoot: string | null;
    /** Persist a navigation (App writes it back into this Finder's tab). */
    onNavigate: (path: string) => void;
    /** Open a file in the workbench (newSplit = Cmd/Ctrl held). */
    onOpenFile: (path: string, newSplit: boolean) => void;
  }

  let { path, wsRoot, onNavigate, onOpenFile }: Props = $props();

  interface Column {
    dir: string;
    entries: FsEntry[];
    /** Path of the highlighted entry in this column, if any. */
    selected: string | null;
  }

  let columns = $state<Column[]>([]);
  /** The canonical current directory (deepest column). Set optimistically so
   *  the reconcile effect doesn't re-fire on our own navigation. */
  let location = $state("");
  let error = $state<string | null>(null);
  let loading = $state(false);
  /** Which column has keyboard focus. */
  let activeCol = $state(0);
  let colsEl = $state<HTMLElement | null>(null);

  // Monotonic guard: a slow list must never clobber a newer navigation.
  let navSeq = 0;
  /** Target of an in-flight navigation, so the reconcile effect stays quiet
   *  while we resolve it (canonicalization can rename the destination). */
  let pending: string | null = null;

  const wsNorm = $derived(
    wsRoot !== null && wsRoot.length > 1 && wsRoot.endsWith("/") ? wsRoot.slice(0, -1) : wsRoot,
  );
  const outsideWs = $derived(location !== "" && !withinWs(location));

  function withinWs(p: string): boolean {
    return wsNorm !== null && (p === wsNorm || p.startsWith(`${wsNorm}/`));
  }

  /** Leftmost column for a location: the workspace root when the path is under
   *  it (so the in-workspace chain shows), else the directory itself. */
  function anchorFor(dir: string): string {
    return wsNorm !== null && withinWs(dir) ? wsNorm : dir;
  }

  /** The dir chain [anchor … dir] (both canonical, dir at/under anchor). */
  function ancestorChain(anchor: string, dir: string): string[] {
    if (dir === anchor) return [anchor];
    const prefix = anchor === "/" ? "/" : `${anchor}/`;
    if (!dir.startsWith(prefix)) return [dir];
    const parts = dir.slice(prefix.length).split("/").filter(Boolean);
    const chain = [anchor];
    let acc = anchor === "/" ? "" : anchor;
    for (const part of parts) {
      acc = `${acc}/${part}`;
      chain.push(acc);
    }
    return chain;
  }

  /** Breadcrumb segments — always the full absolute path from "/". */
  const crumbs = $derived.by(() => {
    const out = [{ name: "/", path: "/" }];
    let acc = "";
    for (const part of location.split("/").filter(Boolean)) {
      acc += `/${part}`;
      out.push({ name: part, path: acc });
    }
    return out;
  });

  function message(e: unknown): string {
    return e instanceof ApiError ? e.message : e instanceof Error ? e.message : "could not read directory";
  }

  /** Full rebuild: resolve `target`, then render the whole anchor→dir chain. */
  async function navigateTo(target: string): Promise<void> {
    if (target === "") return;
    const hidden = getSetting("files.showHidden");
    const seq = ++navSeq;
    pending = target;
    loading = true;
    try {
      const head = await fsList(target, hidden);
      if (seq !== navSeq) return;
      const dir = head.path; // canonical (resolves ~, symlinks, ..)
      const chain = ancestorChain(anchorFor(dir), dir);
      const listings = await Promise.all(
        chain.map((d) => (d === dir ? Promise.resolve(head) : fsList(d, hidden))),
      );
      if (seq !== navSeq) return;
      columns = listings.map((l, i) => ({
        dir: l.path,
        entries: l.entries,
        selected: chain[i + 1] ?? null,
      }));
      location = dir;
      activeCol = columns.length - 1;
      error = null;
      onNavigate(dir);
    } catch (e) {
      if (seq === navSeq) error = message(e);
    } finally {
      if (seq === navSeq) {
        pending = null;
        loading = false;
      }
    }
  }

  /** Descend into (or switch to) `dir` shown in column `colIndex`. */
  async function openDir(colIndex: number, entry: FsEntry): Promise<void> {
    const seq = ++navSeq;
    columns = columns.map((c, i) => (i === colIndex ? { ...c, selected: entry.path } : c));
    location = entry.path; // optimistic; corrected to canonical below
    activeCol = colIndex + 1;
    try {
      const listing = await fsList(entry.path, getSetting("files.showHidden"));
      if (seq !== navSeq) return;
      columns = [
        ...columns.slice(0, colIndex + 1),
        { dir: listing.path, entries: listing.entries, selected: null },
      ];
      location = listing.path;
      activeCol = colIndex + 1;
      error = null;
      onNavigate(listing.path);
    } catch (e) {
      if (seq !== navSeq) return;
      // Couldn't open (permission/gone): drop deeper columns, keep the parent.
      columns = columns.slice(0, colIndex + 1);
      location = columns[colIndex]?.dir ?? location;
      activeCol = colIndex;
      error = message(e);
    }
  }

  /** Select + open a file shown in column `colIndex`. */
  function openFile(colIndex: number, entry: FsEntry, newSplit: boolean): void {
    columns = columns.map((c, i) =>
      i > colIndex ? c : i === colIndex ? { ...c, selected: entry.path } : c,
    );
    columns = columns.slice(0, colIndex + 1);
    activeCol = colIndex;
    onOpenFile(entry.path, newSplit);
  }

  function onRowClick(colIndex: number, entry: FsEntry, e: MouseEvent): void {
    if (entry.kind === "dir") void openDir(colIndex, entry);
    else openFile(colIndex, entry, e.metaKey || e.ctrlKey);
  }

  async function goHome(): Promise<void> {
    try {
      await navigateTo(await fsHome());
    } catch (e) {
      error = message(e);
    }
  }

  // --- keyboard navigation ---------------------------------------------------

  function colSelectedIndex(col: Column): number {
    return col.selected === null ? -1 : col.entries.findIndex((e) => e.path === col.selected);
  }

  function selectInColumn(colIndex: number, entryIndex: number): void {
    const col = columns[colIndex];
    if (col === undefined) return;
    const entry = col.entries[entryIndex];
    if (entry === undefined) return;
    // Highlight without opening; drop any deeper columns so the view matches.
    columns = columns
      .slice(0, colIndex + 1)
      .map((c, i) => (i === colIndex ? { ...c, selected: entry.path } : c));
    activeCol = colIndex;
  }

  function onKeydown(e: KeyboardEvent): void {
    const col = columns[activeCol];
    if (col === undefined) return;
    const cur = colSelectedIndex(col);
    if (e.key === "ArrowDown") {
      e.preventDefault();
      if (col.entries.length > 0) selectInColumn(activeCol, cur < 0 ? 0 : Math.min(cur + 1, col.entries.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      if (col.entries.length > 0) selectInColumn(activeCol, Math.max(cur - 1, 0));
    } else if (e.key === "ArrowLeft") {
      e.preventDefault();
      if (activeCol > 0) activeCol -= 1;
    } else if (e.key === "ArrowRight" || e.key === "Enter") {
      e.preventDefault();
      const entry = col.entries[cur];
      if (entry === undefined) return;
      if (entry.kind === "dir") void openDir(activeCol, entry);
      else if (e.key === "Enter") openFile(activeCol, entry, e.metaKey || e.ctrlKey);
    }
  }

  // --- external nav reconcile ------------------------------------------------

  // Seed on mount and follow external `path` changes (a dir-link redirecting
  // this Finder). Skip when it already matches where we are or are heading —
  // our own navigation reports the canonical dir back through `path`.
  $effect(() => {
    const p = path;
    untrack(() => {
      if (p === "" || p === location || p === pending) return;
      void navigateTo(p);
    });
  });

  // Re-list in place when the show-hidden setting flips.
  let lastHidden = getSetting("files.showHidden");
  $effect(() => {
    const h = getSetting("files.showHidden");
    untrack(() => {
      if (h === lastHidden) return;
      lastHidden = h;
      if (location !== "") void navigateTo(location);
    });
  });

  // Keep the deepest column scrolled into view as the chain grows.
  $effect(() => {
    void columns.length;
    const el = colsEl;
    if (el !== null) untrack(() => (el.scrollLeft = el.scrollWidth));
  });
</script>

<div class="finder">
  <div class="bar">
    <div class="crumbs" role="navigation" aria-label="path">
      {#each crumbs as c, i (c.path)}
        {#if i > 0}<span class="sep">/</span>{/if}
        <button
          class="crumb"
          class:tail={i === crumbs.length - 1}
          title={c.path}
          onclick={() => void navigateTo(c.path)}>{c.name}</button
        >
      {/each}
    </div>
    <div class="actions">
      {#if outsideWs && wsNorm !== null}
        <!-- Not just a marker: click to jump back into the workspace. -->
        <button class="chip" title="back to the workspace" onclick={() => void navigateTo(wsNorm)}>
          <svg viewBox="0 0 24 24" width="11" height="11" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M9 14l-4 -4l4 -4" />
            <path d="M5 10h11a4 4 0 1 1 0 8h-1" />
          </svg>
          outside workspace
        </button>
      {/if}
      {#if wsNorm !== null}
        <button class="act" title="go to the workspace root" onclick={() => void navigateTo(wsNorm)}>workspace</button>
      {/if}
      <button class="act" title="go to your home folder" onclick={goHome}>home</button>
    </div>
  </div>

  <!-- Miller columns. Focusable as a whole; arrows move selection / columns. -->
  <div
    class="cols"
    bind:this={colsEl}
    tabindex="0"
    role="tree"
    aria-label="files"
    onkeydown={onKeydown}
  >
    {#if error !== null && columns.length === 0}
      <div class="pad-error">
        <div class="err-msg">{error}</div>
        <div class="err-actions">
          {#if wsNorm !== null}
            <button class="act" onclick={() => void navigateTo(wsNorm)}>workspace root</button>
          {/if}
          <button class="act" onclick={goHome}>home</button>
        </div>
      </div>
    {/if}
    {#each columns as col, ci (col.dir)}
      <div class="col" class:active={ci === activeCol}>
        {#if col.entries.length === 0}
          <div class="empty">empty</div>
        {/if}
        {#each col.entries as entry (entry.path)}
          <button
            class="row"
            class:sel={entry.path === col.selected}
            title={entry.path}
            onclick={(e) => onRowClick(ci, entry, e)}
          >
            <span class="glyph">
              {#if entry.kind === "dir"}
                <FolderIcon size={14} />
              {:else}
                <FileIcon path={entry.path} size={14} />
              {/if}
            </span>
            <span class="name">{entry.name}</span>
            {#if entry.kind === "dir"}
              <svg class="chev" viewBox="0 0 16 16" width="9" height="9" aria-hidden="true">
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
              <span class="meta">{humanSize(entry.size)}</span>
            {/if}
          </button>
        {/each}
      </div>
    {/each}
    {#if columns.length === 0 && error === null && !loading}
      <div class="empty pad">nothing to show</div>
    {/if}
  </div>

  {#if error !== null && columns.length > 0}
    <div class="footer-error" title={error}>{error}</div>
  {/if}
</div>

<style>
  .finder {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    background: var(--pane-bg, var(--bg));
  }

  .bar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 5px 10px;
    border-bottom: 1px solid var(--edge);
    min-height: 30px;
  }

  .crumbs {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    overflow-x: auto;
    white-space: nowrap;
    scrollbar-width: none;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
  }

  .crumbs::-webkit-scrollbar {
    display: none;
  }

  .sep {
    opacity: 0.5;
    padding: 0 1px;
  }

  .crumb {
    appearance: none;
    border: none;
    background: none;
    padding: 1px 2px;
    font: inherit;
    color: var(--muted);
    cursor: pointer;
    border-radius: 3px;
  }

  .crumb:hover {
    color: var(--fg);
    background: var(--row-hover);
  }

  .crumb.tail {
    color: var(--fg);
  }

  .actions {
    flex: none;
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .chip {
    display: flex;
    align-items: center;
    gap: 4px;
    appearance: none;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    border: 1px solid color-mix(in srgb, var(--accent) 30%, transparent);
    border-radius: 999px;
    padding: 1px 8px 1px 6px;
    white-space: nowrap;
    cursor: pointer;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .chip:hover {
    color: var(--fg);
    background: color-mix(in srgb, var(--accent) 24%, transparent);
  }

  .act {
    appearance: none;
    border: 1px solid var(--edge);
    background: none;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    padding: 2px 7px;
    border-radius: 5px;
    cursor: pointer;
  }

  .act:hover {
    color: var(--fg);
    background: var(--row-hover);
  }

  .cols {
    flex: 1;
    min-height: 0;
    display: flex;
    align-items: stretch;
    overflow-x: auto;
    overflow-y: hidden;
    outline: none;
    scrollbar-width: thin;
    scrollbar-color: color-mix(in srgb, var(--fg) 22%, transparent) transparent;
  }

  .col {
    flex: 0 0 216px;
    min-width: 216px;
    overflow-y: auto;
    border-right: 1px solid var(--edge);
    padding: 4px;
    scrollbar-width: thin;
    scrollbar-color: color-mix(in srgb, var(--fg) 18%, transparent) transparent;
  }

  /* The focused column gets a whisper of emphasis so keyboard nav is legible. */
  .col.active {
    background: color-mix(in srgb, var(--accent) 4%, transparent);
  }

  .row {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 7px;
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    text-align: left;
    color: var(--fg);
    padding: 4px 6px;
    border-radius: 5px;
    cursor: pointer;
    min-width: 0;
  }

  .row:hover {
    background: var(--row-hover);
  }

  .row.sel {
    background: var(--row-active);
  }

  .glyph {
    flex: none;
    display: flex;
    align-items: center;
  }

  .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono);
    font-size: var(--text-sm);
  }

  .chev {
    flex: none;
    color: var(--muted);
    opacity: 0.7;
  }

  .meta {
    flex: none;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    opacity: 0.75;
  }

  .empty {
    padding: 6px;
    font-size: var(--text-xs);
    color: var(--muted);
  }

  .empty.pad {
    margin: auto;
  }

  .pad-error {
    margin: auto;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 10px;
    padding: 16px;
    text-align: center;
  }

  .err-msg {
    font-size: var(--text-sm);
    color: var(--err);
    max-width: 32rem;
  }

  .err-actions {
    display: flex;
    gap: 8px;
  }

  .footer-error {
    flex: none;
    padding: 3px 10px;
    font-size: var(--text-xs);
    color: var(--err);
    border-top: 1px solid var(--edge);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
