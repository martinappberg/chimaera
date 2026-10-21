<script lang="ts">
  import { onMount } from "svelte";
  import { ApiError, getHostLabel } from "../net/api";
  import { openWindow } from "../net/native";
  import { validateEntryName } from "../shared/fsNames";
  import { isMac } from "../shared/keys";
  import { modalFocus } from "../shared/modalFocus";
  import { ancestors, basename, isAbsolute, isRoot, joinPath, rootOf } from "../shared/paths";
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
    /** Present inside a workspace: adds a "Home" row that returns to the
     *  workspace launcher (the caller decides how — it must not close this
     *  window's workspace out from under the user). */
    onHome?: () => void;
  }

  let { recents, onOpened, onClose, onHome }: Props = $props();

  type Row =
    | { kind: "home" }
    | { kind: "recent"; ws: Workspace }
    | { kind: "dir"; dir: DirEntry }
    /** ".." — go up to the parent of the browsed directory. */
    | { kind: "up"; path: string }
    /** The browse-mode "new folder…" affordance (creates in the browsed dir). */
    | { kind: "newfolder" };

  let input = $state("");
  let listing = $state<DirListing | null>(null);
  let error = $state<string | null>(null);
  let highlight = $state(0);
  let navigated = $state(false);
  let listEl = $state<HTMLDivElement | null>(null);
  let crumbsEl = $state<HTMLDivElement | null>(null);
  let filterEl = $state<HTMLInputElement | null>(null);
  let busy = false;

  const showRecents = $derived(!navigated && input === "" && recents.length > 0);
  /** The Home row leads the list until the user starts filtering or browsing. */
  const showHome = $derived(onHome !== undefined && !navigated && input === "");
  const homeOffset = $derived(showHome ? 1 : 0);
  /** The daemon's host names its roots by drive (`C:/`, a Windows host):
   *  its top level `/` is the list of drives, not a folder. */
  const driveHost = $derived(
    listing !== null && (rootOf(listing.path) !== "/" || listing.dirs.some((d) => rootOf(d.path) !== "/")),
  );
  /** An absolute (or ~) path typed into the filter: the footer's Open acts on
   *  IT, not on the breadcrumb directory. On a drive host a typed `C:\Users`
   *  reads as `C:/Users`, and a bare `C:` as its root. */
  const typedPath = $derived.by((): string | null => {
    const t = input.trim();
    if (!driveHost) return t.startsWith("/") || t.startsWith("~") ? t : null;
    const w = t.replaceAll("\\", "/");
    if (/^[A-Za-z]:$/.test(w)) return `${w}/`;
    return isAbsolute(w) || w.startsWith("~") ? w : null;
  });
  /** The drive list is browsable but neither a workspace nor a place to
   *  create a folder, so it has no "open this folder" or "new folder" rows. */
  const driveList = $derived(driveHost && (typedPath ?? listing?.path) === "/");
  const filtered = $derived(
    listing === null || typedPath !== null
      ? []
      : listing.dirs.filter((d) => d.name.toLowerCase().includes(input.trim().toLowerCase())),
  );

  // --- typed-path completion -------------------------------------------------
  // When the filter holds an absolute/~ path, list the children of its deepest
  // directory and match the trailing segment — shell-style tab completion,
  // replacing the old dead-end that only ever offered to open the typed path.
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

  /** The navigable dirs in the list, in whichever mode is
   *  active (typed-path completion vs. filtering the browsed directory). */
  const browseDirs = $derived(typedPath !== null ? pathMatches : filtered);

  /** The typed path doesn't exist yet — the footer's Open becomes "Create & open". Judged from the parent listing (which the
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

  /** The ".." row: browse mode, unfiltered, below the filesystem root. */
  const upPath = $derived(
    typedPath === null && input === "" ? (listing?.parent ?? null) : null,
  );
  const upOffset = $derived(upPath !== null ? 1 : 0);

  const rows = $derived.by((): Row[] => {
    const out: Row[] = [];
    if (showHome) out.push({ kind: "home" });
    if (showRecents) {
      for (const ws of recents) out.push({ kind: "recent", ws });
    }
    if (typedPath !== null || listing !== null) {
      if (upPath !== null) out.push({ kind: "up", path: upPath });
      for (const dir of browseDirs) out.push({ kind: "dir", dir });
      // Browse mode only: create in the directory being browsed (the typed-
      // path mode has its own create — the footer's "Create & open").
      if (typedPath === null && !driveList) out.push({ kind: "newfolder" });
    }
    return out;
  });
  const browseOffset = $derived(homeOffset + (showRecents ? recents.length : 0));
  /** Breadcrumbs, each drawn after a `/` separator. A Unix root is that
   *  leading separator alone; a drive host leads with its drive list. */
  const crumbs = $derived.by((): { name: string; path: string; sep: boolean }[] => {
    if (listing === null) return [];
    const out: { name: string; path: string; sep: boolean }[] = [];
    if (driveHost) out.push({ name: "This PC", path: "/", sep: false });
    for (const p of ancestors(listing.path)) {
      if (p === "/") continue;
      out.push({ name: isRoot(p) ? p.replace(/\/$/, "") : basename(p), path: p, sep: true });
    }
    return out;
  });

  /** The last path segment — what "open" will actually open ("/" for the root). */
  function folderName(p: string): string {
    const t = p.replace(/\/+$/, "");
    return t === "" ? "/" : t.slice(t.lastIndexOf("/") + 1) || t;
  }

  function resetHighlight(): void {
    if (showRecents) {
      highlight = homeOffset; // the first recent, not Home: a stray Enter keeps its old meaning
      return;
    }
    // When completing a path, prefer an exact match of the typed segment so
    // Enter on "/data/lab" descends into lab even if other fuzzy
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
    // Else the first subdirectory (Enter descends); with none, nothing is
    // highlighted and Enter falls through to the footer's Open.
    const firstDir = rows.findIndex((r) => r.kind === "dir");
    highlight = firstDir >= 0 ? firstDir : -1;
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

  /** What the footer's Open acts on: the typed path, else the browsed folder
   *  (nothing on a drive host's drive list, which is not a folder). */
  const target = $derived(driveList ? null : (typedPath ?? listing?.path ?? null));

  /** The footer's primary action (also Cmd/Ctrl+Enter, and Enter when no row
   *  is highlighted): open the target, creating it first when it doesn't exist. */
  function primary(): void {
    if (target === null) return;
    if (typedPathMissing) void createFolder(target);
    else void openHere(target);
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

  // --- browse-mode "new folder…" (create in the browsed directory) -----------
  // Unlike the typed-path create (createFolder above, which opens the new
  // folder as a workspace immediately), this creates and NAVIGATES INTO it —
  // the footer's Open is then one Enter away, and the user can keep nesting.

  let creating = $state(false);
  let createDraft = $state("");

  function startCreate(): void {
    creating = true;
    createDraft = "";
  }

  function cancelCreate(): void {
    creating = false;
    createDraft = "";
    filterEl?.focus();
  }

  async function commitCreate(viaBlur = false): Promise<void> {
    if (!creating) return;
    const name = createDraft.trim();
    if (name === "") {
      cancelCreate();
      return;
    }
    // Slashes allowed: fsMkdir creates parents, so a/b nests — consistent
    // with the inline creates in the tree and Finder.
    const invalid = validateEntryName(name, { allowSlashes: true, dir: listing?.path });
    if (invalid !== null) {
      if (viaBlur) cancelCreate();
      else error = invalid;
      return;
    }
    const base = listing?.path;
    if (base === undefined || busy) return;
    busy = true;
    try {
      const created = await fsMkdir(joinPath(base, name));
      error = null;
      creating = false;
      createDraft = "";
      filterEl?.focus();
      await navigate(created); // fresh folder is empty → nothing highlights, Enter opens it
    } catch (e) {
      error = e instanceof ApiError ? e.message : "could not create folder";
    } finally {
      busy = false;
    }
  }

  function onCreateKeydown(e: KeyboardEvent): void {
    e.stopPropagation();
    if (e.key === "Enter") {
      e.preventDefault();
      void commitCreate();
    } else if (e.key === "Escape") {
      e.preventDefault();
      cancelCreate(); // nulled before blur fires, so blur no-ops
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
      if (row?.kind === "home") {
        onHome?.();
        return;
      }
      if (row?.kind === "up") {
        void navigate(row.path);
        return;
      }
      // The tail row is a mode switch, with or without modifiers.
      if (row?.kind === "newfolder") {
        startCreate();
        return;
      }
      if (row?.kind === "recent") {
        void openHere(row.ws.root);
        return;
      }
      // Cmd/Ctrl+Enter always opens what the footer names; plain Enter follows
      // the highlight (a directory descends — how a typed path is entered) and
      // opens the footer's target when nothing is highlighted.
      if (e.metaKey || e.ctrlKey || row?.kind !== "dir") {
        primary();
      } else {
        void navigate(row.dir.path);
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
          // Parent doesn't exist yet: no completions — the footer offers to
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

<div class="overlay">
  <button class="scrim" aria-label="close" tabindex="-1" onclick={onClose}></button>
  <div
    class="panel"
    role="dialog"
    aria-modal="true"
    aria-label="open folder"
    tabindex="-1"
    use:modalFocus
  >
    <input
      class="filter"
      bind:this={filterEl}
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
          {#each crumbs as c, ci (c.path)}
            {#if c.sep}<span class="sep">/</span>{/if}
            <button class="crumb" class:current={typedPath === null && ci === crumbs.length - 1} tabindex="-1" onmousedown={keepFocus} onclick={() => void navigate(c.path)}
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
      {#if showHome}
        <div
          class="rowwrap home-row"
          role="presentation"
          class:hl={highlight === 0}
          data-idx={0}
          onmouseenter={() => (highlight = 0)}
        >
          <button class="row" tabindex="-1" onmousedown={keepFocus} onclick={() => onHome?.()}>
            <svg class="home-ico" viewBox="0 0 16 16" width="13" height="13" aria-hidden="true">
              <path d="M2.5 7.5 8 3l5.5 4.5M4 6.7V13h3v-3.5h2V13h3V6.7" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" />
            </svg>
            <span class="name here-label">Home</span>
            <span class="home-hint">all workspaces</span>
          </button>
        </div>
      {/if}
      {#if showRecents}
        <div class="section">recent</div>
        {#each recents as ws, i (ws.id)}
          {@const ri = i + homeOffset}
          <div
            class="rowwrap"
            role="presentation"
            class:hl={highlight === ri}
            data-idx={ri}
            onmouseenter={() => (highlight = ri)}
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
          </div>
        {/each}
        <div class="section">browse</div>
      {/if}
      {#if typedPath !== null || listing !== null}
        {#if upPath !== null}
          {@const ui = browseOffset}
          <div
            class="rowwrap"
            role="presentation"
            class:hl={highlight === ui}
            data-idx={ui}
            onmouseenter={() => (highlight = ui)}
          >
            <button
              class="row"
              tabindex="-1"
              title={upPath}
              onmousedown={keepFocus}
              onclick={() => void navigate(upPath)}
            >
              <span class="name up-name">..</span>
              <span class="up-hint">{folderName(upPath)}</span>
            </button>
          </div>
        {/if}
        {#each browseDirs as dir, j (dir.path)}
          {@const idx = browseOffset + upOffset + j}
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
          </div>
        {/each}
        {#if typedPath === null && listing !== null && !driveList}
          {@const nfIdx = browseOffset + upOffset + browseDirs.length}
          <div
            class="rowwrap"
            role="presentation"
            class:hl={highlight === nfIdx && !creating}
            data-idx={nfIdx}
            onmouseenter={() => (highlight = nfIdx)}
          >
            {#if creating}
              <div class="row creating">
                <input
                  class="create-input"
                  type="text"
                  placeholder="folder name — a/b nests"
                  spellcheck="false"
                  autocomplete="off"
                  aria-label="new folder name"
                  bind:value={createDraft}
                  use:focusOnMount
                  onkeydown={onCreateKeydown}
                  onblur={() => void commitCreate(true)}
                />
              </div>
            {:else}
              <button
                class="row"
                tabindex="-1"
                title={`new folder in ${listing.path}`}
                onmousedown={keepFocus}
                onclick={startCreate}
              >
                <span class="name here-label create">new folder…</span>
              </button>
            {/if}
          </div>
        {/if}
        {#if listTruncated}
          <div class="trunc">large folder — showing the first {browseDirs.length}. narrow your filter.</div>
        {/if}
      {/if}
    </div>
    {#if target !== null}
      <div class="foot">
        <button
          class="primary"
          tabindex="-1"
          title={target}
          onmousedown={keepFocus}
          onclick={primary}
        >
          <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true">
            <path
              d="M2 4.5A1.5 1.5 0 0 1 3.5 3h2.6l1.4 1.5h5A1.5 1.5 0 0 1 14 6v5.5a1.5 1.5 0 0 1-1.5 1.5h-9A1.5 1.5 0 0 1 2 11.5z"
              fill="none"
              stroke="currentColor"
              stroke-width="1.3"
              stroke-linejoin="round"
            />
          </svg>
          <span class="verb">{typedPathMissing ? "Create & open" : "Open"}</span>
          <span class="who">{folderName(target)}</span>
          <kbd>{isMac ? "⌘↵" : "Ctrl+↵"}</kbd>
        </button>
        {#if !typedPathMissing}
          <button
            class="alt"
            tabindex="-1"
            onmousedown={keepFocus}
            onclick={() => void openNewWindow(target)}>Open in new window</button
          >
        {/if}
      </div>
    {/if}
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

  .row.creating {
    cursor: default;
    padding: 4px 8px;
  }

  /* Inline new-folder input, sized like the row text it replaces. */
  .create-input {
    flex: 1;
    min-width: 0;
    font: inherit;
    font-size: var(--text-md);
    color: var(--fg);
    background: var(--bg);
    border: 1px solid color-mix(in srgb, var(--accent) 45%, var(--edge));
    border-radius: 4px;
    padding: 2px 6px;
    outline: none;
  }

  .create-input::placeholder {
    color: var(--muted);
    opacity: 0.6;
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

  /* "new folder…" — accent-coloured so the make-a-directory row reads apart
     from the plain navigation rows. */
  .name.here-label.create {
    color: var(--accent);
  }

  .crumb.current {
    color: var(--fg);
    font-weight: 600;
  }

  .chev {
    flex: none;
    color: var(--muted);
    opacity: 0.6;
  }

  .foot {
    flex: none;
    display: flex;
    justify-content: flex-end;
    gap: 6px;
    padding: 6px 8px;
    border-top: 1px solid var(--edge);
  }

  /* The one commit action, named after the folder it will open. */
  .primary {
    flex: 0 1 auto;
    min-width: 0;
    max-width: 70%;
    display: flex;
    align-items: center;
    gap: 6px;
    appearance: none;
    border: 1px solid color-mix(in srgb, var(--accent) 30%, var(--edge));
    border-radius: 5px;
    background: color-mix(in srgb, var(--accent) 9%, transparent);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
    padding: 4px 10px;
    cursor: pointer;
    text-align: left;
    transition: background-color 0.12s ease;
  }

  .primary:hover {
    background: color-mix(in srgb, var(--accent) 16%, transparent);
  }

  .primary svg {
    flex: none;
    color: var(--accent);
  }

  .primary .verb {
    flex: none;
    color: var(--muted);
  }

  .primary .who {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono);
    font-weight: 600;
  }

  .primary kbd {
    flex: none;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
  }

  .alt {
    order: -1;
    flex: none;
    appearance: none;
    border: 1px solid var(--edge);
    border-radius: 5px;
    background: none;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-sm);
    padding: 4px 10px;
    cursor: pointer;
    white-space: nowrap;
  }

  .alt:hover {
    color: var(--fg);
    background: var(--row-active);
  }

  .home-ico { flex: none; color: var(--muted); }
  .home-row.hl .home-ico { color: var(--fg); }
  .home-hint { margin-left: auto; font-size: var(--text-xs); color: var(--muted); }
</style>
