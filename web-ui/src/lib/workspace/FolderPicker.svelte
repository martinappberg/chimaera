<script lang="ts">
  import { onMount } from "svelte";
  import { ApiError, getHostLabel } from "../net/api";
  import { openWindow } from "../net/native";
  import {
    createWorkspace,
    fsDirs,
    fsHome,
    fsMkdir,
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
  /** An absolute (or ~) path typed into the filter: the "open this folder"
   *  row acts on IT, not on the breadcrumb directory. */
  const typedPath = $derived.by((): string | null => {
    const t = input.trim();
    return t.startsWith("/") || t.startsWith("~") ? t : null;
  });
  const filtered = $derived(
    listing === null || typedPath !== null
      ? []
      : listing.dirs.filter((d) => d.name.toLowerCase().includes(input.trim().toLowerCase())),
  );

  // --- typed-path completion -------------------------------------------------
  // When the filter holds an absolute/~ path, list the children of its deepest
  // directory and match the trailing segment — shell-style tab completion,
  // replacing the old dead-end that only ever offered "open this folder".
  let pathDirs = $state<DirEntry[]>([]);
  /** The directory `pathDirs` was listed from (dedupes refetches per keystroke). */
  let pathBase = $state("");
  /** The typed path's directory was capped by the daemon. */
  let pathTruncated = $state(false);
  /** Whether the typed path's directory (`pathBase`) was listed successfully —
   *  false means it doesn't exist yet, which drives the "create folder" offer. */
  let pathBaseExists = $state(false);
  /** Monotonic guard so a slow listing can't clobber a newer one. */
  let pathSeq = 0;

  /** Split a typed path into (dir, trailing segment). */
  function splitTyped(p: string): { dir: string; tail: string } {
    const slash = p.lastIndexOf("/");
    if (slash < 0) return { dir: p, tail: "" };
    return { dir: p.slice(0, slash + 1), tail: p.slice(slash + 1) };
  }

  /** Case-insensitive subsequence match (both args already lowercased). */
  function subseq(needle: string, hay: string): boolean {
    if (needle === "") return true;
    let i = 0;
    for (const c of hay) {
      if (c === needle[i] && ++i === needle.length) return true;
    }
    return false;
  }

  /** Children of the typed path's directory, filtered by its trailing segment. */
  const pathMatches = $derived.by((): DirEntry[] => {
    if (typedPath === null) return [];
    const t = splitTyped(typedPath).tail.toLowerCase();
    return t === "" ? pathDirs : pathDirs.filter((d) => subseq(t, d.name.toLowerCase()));
  });

  /** The navigable dirs under the "open this folder" row, in whichever mode is
   *  active (typed-path completion vs. filtering the browsed directory). */
  const browseDirs = $derived(typedPath !== null ? pathMatches : filtered);

  /** The typed path doesn't exist yet — the top row becomes "create folder"
   *  instead of "open this folder". Judged from the parent listing (which the
   *  completion effect already fetches): once it has settled for this path's
   *  directory, the path is missing when a trailing segment matches no child,
   *  or — for a path ending in "/" — when the directory itself couldn't be
   *  listed. Unknown (still fetching) reads as present, so no premature offer. */
  const typedPathMissing = $derived.by((): boolean => {
    if (typedPath === null) return false;
    const { dir, tail } = splitTyped(typedPath);
    if (pathBase !== dir) return false; // parent listing not settled yet
    if (tail === "") return !pathBaseExists; // "…/": the dir itself is the target
    return !pathDirs.some((d) => d.name === tail);
  });

  /** The active directory was capped by the daemon (very large folder). */
  const listTruncated = $derived(typedPath !== null ? pathTruncated : (listing?.truncated ?? false));

  const rows = $derived.by((): Row[] => {
    const out: Row[] = [];
    if (showRecents) {
      for (const ws of recents) out.push({ kind: "recent", ws });
    }
    const here = typedPath ?? listing?.path ?? null;
    if (here !== null) {
      out.push({ kind: "here", path: here });
      for (const dir of browseDirs) out.push({ kind: "dir", dir });
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
    // When completing a path, prefer an exact match of the typed segment so
    // Enter on "/oak/stanford" descends into stanford even if other fuzzy
    // matches sort ahead of it.
    if (typedPath !== null) {
      const tail = splitTyped(typedPath).tail.toLowerCase();
      if (tail !== "") {
        const exact = rows.findIndex(
          (r) => r.kind === "dir" && r.dir.name.toLowerCase() === tail,
        );
        if (exact >= 0) {
          highlight = exact;
          return;
        }
      }
    }
    // Else the first subdirectory (Enter descends); fall back to "open this
    // folder" when nothing matches.
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

  /** Create a not-yet-existing folder (and any missing parents), then open it
   *  as a workspace in THIS window — the "create folder" affordance for a
   *  typed path that doesn't exist. */
  async function createFolder(path: string | null): Promise<void> {
    if (path === null || busy) return;
    busy = true;
    try {
      onOpened(await createWorkspace(await fsMkdir(path)));
    } catch (e) {
      error = e instanceof ApiError ? e.message : "could not create folder";
    } finally {
      busy = false;
    }
  }

  async function openNewWindow(path: string | null): Promise<void> {
    if (path === null || busy) return;
    busy = true;
    try {
      const w = await createWorkspace(path);
      // A real native window in the shell, a new tab in the browser. Force a
      // new window (newWindow=true): this is the explicit "new window" action,
      // so it must never be diverted to raise an already-open one. Target THIS
      // window's own daemon — `createWorkspace` made the folder on the daemon
      // serving this window (remote when this picker runs in a remote window),
      // so a hardcoded local `null` would open a local window for a workspace
      // the local daemon doesn't have and bounce to the launcher.
      const alias = getHostLabel() === "local" ? null : getHostLabel();
      await openWindow(alias, w.id, true);
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
      const row = rows[highlight];
      // Cmd/Ctrl+Enter always opens the highlighted target as a workspace —
      // creating the folder first when the typed path doesn't exist yet.
      if (e.metaKey || e.ctrlKey) {
        if (row?.kind === "here" && typedPathMissing) void createFolder(row.path);
        else void openHere(rowPath(row));
        return;
      }
      // Follow the highlight: a directory descends (this is how a typed path
      // is entered — its matching completion is highlighted); the top row
      // opens the current/typed path as a workspace, or creates it first when
      // it doesn't exist.
      if (row?.kind === "dir") {
        void navigate(row.dir.path);
      } else if (row?.kind === "here" && typedPathMissing) {
        void createFolder(row.path);
      } else if (row !== undefined) {
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

  // Fetch the typed path's directory (debounced) so completions appear as you
  // type. `pathSeq` drops stale responses; the cleanup cancels a pending fetch
  // when the path changes again, so keystrokes never stack up requests.
  $effect(() => {
    const tp = typedPath;
    if (tp === null) {
      pathDirs = [];
      pathBase = "";
      return;
    }
    const { dir } = splitTyped(tp);
    if (dir === pathBase) return; // same directory already listed — just refilter
    const seq = ++pathSeq;
    const timer = setTimeout(() => {
      void fsDirs(dir)
        .then((l) => {
          if (seq !== pathSeq) return;
          pathDirs = l.dirs;
          pathTruncated = l.truncated ?? false;
          pathBaseExists = true;
          pathBase = dir;
          resetHighlight();
        })
        .catch(() => {
          if (seq !== pathSeq) return;
          // Parent doesn't exist yet: no completions — the top row offers to
          // create the typed path instead of opening it.
          pathDirs = [];
          pathTruncated = false;
          pathBaseExists = false;
          pathBase = dir;
          resetHighlight();
        });
    }, 120);
    return () => clearTimeout(timer);
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
          <div
            class="rowwrap"
            role="presentation"
            class:hl={highlight === i}
            data-idx={i}
            onmouseenter={() => (highlight = i)}
          >
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
      {#if typedPath !== null || listing !== null}
        {@const herePath = typedPath ?? listing?.path ?? null}
        {#if herePath !== null}
          <div
            class="rowwrap"
            role="presentation"
            class:hl={highlight === browseOffset}
            data-idx={browseOffset}
            onmouseenter={() => (highlight = browseOffset)}
          >
            <button
              class="row"
              tabindex="-1"
              title={typedPathMissing ? `create ${herePath}` : herePath}
              onmousedown={keepFocus}
              onclick={() =>
                typedPathMissing ? void createFolder(herePath) : void openHere(herePath)}
            >
              <span class="name here-label" class:create={typedPathMissing}>
                {typedPathMissing ? "create folder" : "open this folder"}
              </span>
              {#if typedPath !== null}
                <span class="here-path">{typedPath}</span>
              {/if}
            </button>
            {#if !typedPathMissing}
              {@render newWindow(herePath)}
            {/if}
          </div>
        {/if}
        {#each browseDirs as dir, j (dir.path)}
          {@const idx = browseOffset + 1 + j}
          <div
            class="rowwrap"
            role="presentation"
            class:hl={highlight === idx}
            data-idx={idx}
            onmouseenter={() => (highlight = idx)}
          >
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
        {#if listTruncated}
          <div class="trunc">large folder — showing the first {browseDirs.length}. narrow your filter.</div>
        {/if}
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
    font-size: var(--text-md);
    padding: 12px 16px 8px;
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
    padding: 0 16px 8px;
    border-bottom: 1px solid var(--edge);
    font-family: var(--mono);
    font-size: var(--text-xs);
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
    padding: 4px 8px 8px;
    scrollbar-width: thin;
    scrollbar-color: color-mix(in srgb, var(--fg) 22%, transparent) transparent;
  }

  .section {
    padding: 8px 8px 2px;
    font-size: var(--text-xs);
    color: var(--muted);
  }

  .error {
    padding: 8px;
    font-size: var(--text-sm);
    color: var(--err);
  }

  .trunc {
    padding: 6px 8px 2px;
    font-size: var(--text-xs);
    color: var(--muted);
  }

  .rowwrap {
    display: flex;
    align-items: center;
    border-radius: 5px;
    transition: background-color 0.12s ease;
  }

  /* One highlight at a time: mouse hover MOVES the keyboard highlight (see
     onmouseenter) instead of painting a second row. */
  .rowwrap.hl {
    background: var(--row-active);
  }

  .row {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 8px;
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-md);
    color: var(--fg);
    text-align: left;
    padding: 6px 8px;
    cursor: pointer;
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

  .name.here-label {
    flex: none;
  }

  /* "create folder" — distinguish the make-a-new-directory action from the
     plain "open this folder" so it's clearly not just opening what you typed. */
  .name.here-label.create {
    color: var(--accent);
  }

  .here-path {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono);
    font-size: var(--text-sm);
    color: var(--muted);
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
    font-size: var(--text-xs);
    color: var(--muted);
    padding: 0.2rem 8px;
    cursor: pointer;
  }

  .side:hover {
    color: var(--fg);
  }

  .rowwrap.hl .side {
    display: block;
  }
</style>
