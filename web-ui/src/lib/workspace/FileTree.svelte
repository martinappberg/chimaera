<script lang="ts">
  /**
   * Lazy directory tree for the rail's FILES section. Each expanded dir is
   * one /fs/list call; listings are cached per path and refreshed every time
   * the dir is re-expanded. The tree renders flat rows (indent by depth) —
   * no recursive components, trivial scrolling.
   */
  import { tick, untrack } from "svelte";
  import { fsDownload, fsList, type FsEntry } from "../previews/files";
  import { getSetting } from "../settings/store.svelte";
  import { gitIndex, gitStatus } from "./git";
  import { decoFor, dirColor } from "./gitDeco";
  import { fsCreateOp, fsEpoch, fsRenameOp, requestDelete } from "./fsEvents";
  import { stemLength, validateEntryName } from "../shared/fsNames";
  import { contextMenu, type ContextMenuEntry } from "../shared/contextMenu.svelte";
  import { writeClipboard } from "../net/native";
  import FileIcon from "../shared/FileIcon.svelte";
  import FolderIcon from "../shared/FolderIcon.svelte";
  import Spinner from "../previews/Spinner.svelte";

  interface Props {
    /** Workspace root on the daemon's filesystem. */
    root: string;
    /** Open a file surface in the layout. */
    onOpen(path: string): void;
    /** Begin a pointer drag of a tree entry — file OR dir (same grammar as
     *  rail rows and pane tabs). `onEntryClick` is the sub-threshold action
     *  (open for files, expand/collapse for dirs), routed through the drag so
     *  a completed drag never ALSO fires the row's click. */
    onDragStart(e: PointerEvent, path: string, kind: "file" | "dir", onEntryClick: () => void): void;
    /** The focused pane's active file, for the subtle current marker. */
    activePath: string | null;
    /**
     * Reveal request (terminal dir links): expand the ancestor chain of
     * `path` and scroll it into view. The nonce distinguishes repeats.
     */
    reveal?: { path: string; nonce: number } | null;
    /**
     * Inline-create request from the rail-section header buttons (targets the
     * workspace root). The nonce distinguishes repeats.
     */
    createRequest?: { kind: "file" | "dir"; nonce: number } | null;
  }

  let {
    root,
    onOpen,
    onDragStart,
    activePath,
    reveal = null,
    createRequest = null,
  }: Props = $props();

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

  // files.showHidden toggles re-list every visible dir in place (expansion
  // and scroll survive; a hidden dir that vanishes just prunes its subtree).
  let lastHidden = getSetting("files.showHidden");
  $effect(() => {
    const hidden = getSetting("files.showHidden");
    untrack(() => {
      if (hidden === lastHidden) return;
      lastHidden = hidden;
      void load(root);
      for (const dir of expanded) void load(dir);
    });
  });

  // A git epoch bump means files may have APPEARED or vanished (an agent wrote
  // a new file, a checkout removed one). Re-list every visible dir so the tree
  // matches the status overlay — otherwise a brand-new untracked file carries a
  // status the tree has no row to show it on. Plain `let` (not $state): written
  // inside the effect that reads the epoch.
  let lastGitEpoch = -1;
  $effect(() => {
    const epoch = $gitStatus?.epoch ?? -1;
    untrack(() => {
      if (epoch < 0 || epoch === lastGitEpoch) return;
      const first = lastGitEpoch < 0;
      lastGitEpoch = epoch;
      if (first) return; // the initial fetch's listing is already current
      void load(root);
      for (const dir of expanded) void load(dir);
    });
  });

  async function load(dir: string): Promise<void> {
    loading = new Set(loading).add(dir);
    try {
      const listing = await fsList(dir, getSetting("files.showHidden"));
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
    if (edit?.mode === "rename" && edit.path === entry.path) return; // the input owns keys
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      if (entry.kind === "dir") toggle(entry.path);
      else onOpen(entry.path);
    } else if (e.key === "F2") {
      e.preventDefault();
      beginRename(entry);
    }
  }

  // --- inline create/rename + the context menu -------------------------------

  /** The one in-flight inline edit (create under a dir, or rename a row). */
  let edit = $state<
    | { mode: "create"; kind: "file" | "dir"; parent: string }
    | { mode: "rename"; path: string; name: string; kind: "file" | "dir" }
    | null
  >(null);
  let editDraft = $state("");
  let editError = $state<string | null>(null);

  /** Where the create row renders: after this row index; -1 = top (root). */
  const createAfterIndex = $derived.by(() => {
    if (edit?.mode !== "create" || edit.parent === root) return -1;
    const parent = edit.parent;
    return rows.findIndex((r) => r.entry.path === parent);
  });

  function beginCreate(kind: "file" | "dir", parent: string): void {
    closeFilter();
    if (parent !== root && !expanded.has(parent)) {
      expanded = new Set(expanded).add(parent);
      void load(parent);
    }
    edit = { mode: "create", kind, parent };
    editDraft = "";
    editError = null;
  }

  function beginRename(entry: FsEntry): void {
    closeFilter();
    edit = { mode: "rename", path: entry.path, name: entry.name, kind: entry.kind };
    editDraft = entry.name;
    editError = null;
  }

  function cancelEdit(): void {
    edit = null;
    editDraft = "";
    editError = null;
  }

  /** Focus the fresh inline input; renames preselect the stem. */
  function editFocus(node: HTMLInputElement, selectStem: boolean): void {
    node.focus();
    if (selectStem) node.setSelectionRange(0, stemLength(node.value));
  }

  /** Commit the inline edit. `viaBlur` demotes a validation error to a
   *  cancel — never a floating error beside an unfocused ghost input. */
  async function commitEdit(viaBlur = false): Promise<void> {
    const cur = edit;
    if (cur === null) return;
    const name = editDraft.trim();
    if (name === "") {
      cancelEdit();
      return;
    }
    const invalid = validateEntryName(name, { allowSlashes: cur.mode === "create" });
    if (invalid !== null) {
      if (viaBlur) cancelEdit();
      else editError = invalid;
      return;
    }
    try {
      if (cur.mode === "create") {
        const base = cur.parent === "/" ? "" : cur.parent;
        const created = await fsCreateOp(`${base}/${name}`, cur.kind);
        cancelEdit();
        // doReveal refreshes every ancestor listing (nested a/b/c names just
        // work), expands the chain, scrolls + flashes the new row.
        await doReveal(created);
        if (cur.kind === "file") onOpen(created);
      } else {
        if (name === cur.name) {
          cancelEdit();
          return;
        }
        const parent = parentOf(cur.path);
        await fsRenameOp(cur.path, `${parent === "/" ? "" : parent}/${name}`);
        cancelEdit(); // the fsEpoch bump re-lists; App rewrites open tabs
      }
    } catch (err) {
      editError = err instanceof Error ? err.message : "failed";
    }
  }

  function onEditKeydown(e: KeyboardEvent): void {
    e.stopPropagation(); // keep row/tree handlers and app chords away
    if (e.key === "Enter") {
      e.preventDefault();
      void commitEdit();
    } else if (e.key === "Escape") {
      e.preventDefault();
      cancelEdit(); // nulling first makes the following blur a no-op
    }
  }

  async function copyPath(path: string): Promise<void> {
    if (await writeClipboard(path)) return;
    try {
      await navigator.clipboard.writeText(path);
    } catch {
      // clipboard unavailable — quiet, like the terminal's copy path
    }
  }

  function menuFor(entry: FsEntry): ContextMenuEntry[] {
    const dirTarget = entry.kind === "dir" ? entry.path : parentOf(entry.path);
    return [
      { label: "New File…", onSelect: () => beginCreate("file", dirTarget) },
      { label: "New Folder…", onSelect: () => beginCreate("dir", dirTarget) },
      "separator",
      { label: "Rename…", onSelect: () => beginRename(entry) },
      "separator",
      { label: "Download", onSelect: () => void fsDownload(entry.path) },
      { label: "Copy Path", onSelect: () => void copyPath(entry.path) },
      "separator",
      {
        label: "Delete…",
        danger: true,
        onSelect: () => requestDelete(entry.path, entry.kind),
      },
    ];
  }

  // Header-button create requests target the workspace root.
  $effect(() => {
    const req = createRequest;
    if (req == null) return;
    void req.nonce; // track repeats
    untrack(() => beginCreate(req.kind, root));
  });

  // Any fs mutation (this tree, the Finder, a tab rename) re-lists every
  // visible dir — same shape as the git-epoch refresh above, and the only
  // channel for paths outside a git repo.
  let lastFsEpoch = 0;
  $effect(() => {
    const epoch = $fsEpoch;
    untrack(() => {
      if (epoch === lastFsEpoch) return;
      lastFsEpoch = epoch;
      if (epoch === 0) return;
      void load(root);
      for (const dir of expanded) void load(dir);
    });
  });
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
    oncontextmenu={(e) =>
      contextMenu.openAt(e, [
        { label: "New File…", onSelect: () => beginCreate("file", root) },
        { label: "New Folder…", onSelect: () => beginCreate("dir", root) },
      ])}
  >
  {#if rootError !== null}
    <div class="tree-error">{rootError}</div>
  {:else if listings.get(root) === undefined}
    <!-- First listing still in flight (a big dir over ssh takes a while) —
         the delayed spinner keeps fast local opens flicker-free. -->
    <div class="tree-loading"><Spinner label="listing files…" /></div>
  {:else if listings.get(root)?.length === 0 && edit === null}
    <div class="tree-empty">empty</div>
  {:else if filterQuery !== "" && rows.length === 0}
    <div class="tree-empty">no matches</div>
  {/if}
  {#snippet createRow(depth: number)}
    {#if edit?.mode === "create"}
      <div class="node edit-node" style:padding-left={`${8 + depth * 13}px`}>
        <span class="chev-spacer" aria-hidden="true"></span>
        <span class="row-glyph">
          {#if edit.kind === "dir"}
            <FolderIcon open={false} size={14} />
          {:else}
            <FileIcon path={editDraft} size={14} />
          {/if}
        </span>
        <!-- svelte-ignore a11y_autofocus -->
        <input
          class="edit-input"
          type="text"
          spellcheck="false"
          autocomplete="off"
          aria-label={edit.kind === "dir" ? "new folder name" : "new file name"}
          placeholder={edit.kind === "dir" ? "folder name" : "name.ext — a/b nests"}
          bind:value={editDraft}
          use:editFocus={false}
          onkeydown={onEditKeydown}
          onblur={() => void commitEdit(true)}
        />
      </div>
      {#if editError !== null}
        <div class="edit-error" style:padding-left={`${8 + depth * 13}px`}>{editError}</div>
      {/if}
    {/if}
  {/snippet}
  {#if edit?.mode === "create" && createAfterIndex === -1}
    {@render createRow(0)}
  {/if}
  {#each rows as { entry, depth }, i (entry.path)}
    {@const gEntry = entry.kind === "file" ? $gitIndex.files.get(entry.path) : undefined}
    {@const gDeco = gEntry ? decoFor(gEntry) : null}
    {@const gDir = entry.kind === "dir" ? $gitIndex.dirs.get(entry.path) : undefined}
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
      onpointerdowncapture={(e) => {
        // The rename input stays a plain interactive target (rail-row idiom).
        if (e.target instanceof Element && e.target.closest(".edit-input")) return;
        // Both kinds click via the drag's sub-threshold path — a DOM onclick
        // would ALSO fire after a completed drag (pointer capture retargets
        // the click back to this row) and double-act. Skip while renaming.
        onDragStart(e, entry.path, entry.kind, () => {
          if (edit?.mode === "rename" && edit.path === entry.path) return;
          if (entry.kind === "dir") toggle(entry.path);
          else onOpen(entry.path);
        });
      }}
      onkeydown={(e) => onRowKey(e, entry)}
      oncontextmenu={(e) => contextMenu.openAt(e, menuFor(entry))}
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
        <span class="chev-spacer" aria-hidden="true"></span>
      {/if}
      <span class="row-glyph">
        {#if entry.kind === "dir"}
          <FolderIcon open={expanded.has(entry.path)} size={14} />
        {:else}
          <FileIcon path={entry.path} size={14} />
        {/if}
      </span>
      {#if edit?.mode === "rename" && edit.path === entry.path}
        <!-- svelte-ignore a11y_autofocus -->
        <input
          class="edit-input"
          type="text"
          spellcheck="false"
          autocomplete="off"
          aria-label="rename to"
          bind:value={editDraft}
          use:editFocus={true}
          onkeydown={onEditKeydown}
          onblur={() => void commitEdit(true)}
        />
      {:else}
        <span
          class="node-name"
          class:dir={entry.kind === "dir"}
          style:color={gDeco ? gDeco.color : undefined}>{entry.name}</span>
        {#if gDeco}
          <span class="git-badge" style:color={gDeco.color} title={gDeco.label}
            >{gDeco.letter}</span>
        {:else if gDir}
          <span
            class="git-dot"
            style:background={dirColor(gDir)}
            title="contains changes"
            aria-hidden="true"
          ></span>
        {/if}
      {/if}
    </div>
    {#if edit?.mode === "rename" && edit.path === entry.path && editError !== null}
      <div class="edit-error" style:padding-left={`${8 + depth * 13}px`}>{editError}</div>
    {/if}
    {#if edit?.mode === "create" && createAfterIndex === i}
      {@render createRow(depth + 1)}
    {/if}
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
    gap: 4px;
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

  /* A dir whose listing is in flight: nothing for the first beat (fast local
     expands never flicker), then a soft pulse for slow (remote) ones. */
  .chev.busy {
    animation: chev-wait 1.1s ease-in-out 0.25s infinite;
  }

  @keyframes chev-wait {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0.25;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .chev.busy {
      animation: none;
      opacity: 0.4;
    }
  }

  .tree-loading {
    position: relative;
    min-height: 96px;
    flex: none;
  }

  /* Blank disclosure slot for files, so their folder/file glyph aligns under
     the folder icons of sibling directories (fixed chevron column). */
  .chev-spacer {
    flex: none;
    width: 9px;
  }

  /* The folder/file glyph column, a hair tighter to the disclosure. */
  .row-glyph {
    flex: none;
    display: flex;
    align-items: center;
    margin-left: -1px;
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

  /* Inline create/rename input, sized like the name it replaces. */
  .edit-input {
    flex: 1;
    min-width: 0;
    font-family: var(--mono);
    font-size: 0.74rem;
    color: var(--fg);
    background: var(--bg);
    border: 1px solid color-mix(in srgb, var(--accent) 45%, var(--edge));
    border-radius: 3px;
    padding: 0 4px;
    height: 18px;
    outline: none;
  }

  .edit-input::placeholder {
    color: var(--muted);
    opacity: 0.6;
  }

  .edit-node {
    cursor: default;
  }

  .edit-error {
    font-family: var(--mono);
    font-size: 0.68rem;
    color: var(--err);
    padding-top: 1px;
    padding-bottom: 2px;
    white-space: normal;
    word-break: break-word;
  }

  .node-name.dir {
    color: var(--fg);
  }

  .node.active .node-name {
    color: var(--fg);
  }

  /* Git status: a single-letter badge (files) or a rollup dot (collapsed dirs),
     pushed to the row's right edge — quiet, only present when state matters. */
  .git-badge {
    flex: none;
    margin-left: auto;
    font-family: var(--mono);
    font-size: 0.66rem;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    line-height: 1;
  }
  .git-dot {
    flex: none;
    margin-left: auto;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    opacity: 0.85;
  }
</style>
