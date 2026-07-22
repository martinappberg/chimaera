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
  import { tick, untrack } from "svelte";
  import { basename, fsDownload, fsList, type FsEntry, humanSize } from "./files";
  import { fsHome } from "../workspace/sessions";
  import { getSetting } from "../settings/store.svelte";
  import { ApiError, isRemoteHost } from "../net/api";
  import {
    fsCreateOp,
    fsEpoch,
    fsRenameOp,
    lastFsMutation,
    requestDelete,
  } from "../workspace/fsEvents";
  import {
    clearDiskDirs,
    lastDiskChange,
    setDiskDirs,
  } from "../workspace/diskWatch";
  import {
    clearClip,
    copyFile,
    cutFile,
    fileClip,
    isCutPending,
    pasteInto,
  } from "../workspace/fileClipboard.svelte";
  import { stemLength, validateEntryName } from "../shared/fsNames";
  import { contextMenu, type ContextMenuEntry } from "../shared/contextMenu.svelte";
  import { writeClipboard } from "../net/native";
  import FileIcon from "../shared/FileIcon.svelte";
  import FolderIcon from "../shared/FolderIcon.svelte";
  import Spinner from "./Spinner.svelte";

  /** Local-daemon windows hide Download (the file already lives here). */
  const remote = isRemoteHost();

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
    truncated: boolean;
    /** Path of the highlighted entry in this column, if any. */
    selected: string | null;
  }

  let columns = $state<Column[]>([]);
  const diskDirOwner = {};

  // A kept-alive Finder monitors exactly the columns it preserves. Returning
  // to the tab is still instant, while directories outside the workspace and
  // outside Git remain coherent.
  $effect(() => setDiskDirs(diskDirOwner, columns.map((column) => column.dir)));
  $effect(() => () => clearDiskDirs(diskDirOwner));
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
  /** An in-flight DESCEND: a placeholder "incoming column" spinner shows to the
   *  right of column `afterIndex` until its listing settles (macOS Finder). */
  let pendingCol = $state<{ afterIndex: number; seq: number } | null>(null);

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
    pendingCol = null; // a full navigation supersedes any in-flight descend
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
        truncated: l.truncated === true,
        selected: chain[i + 1] ?? null,
      }));
      location = dir;
      activeCol = columns.length - 1;
      error = null;
      onNavigate(dir);
      await tick();
      revealColumn(columns.length - 1);
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
    if (entry.broken) return; // a dangling symlink leads nowhere
    const seq = ++navSeq;
    columns = columns.map((c, i) => (i === colIndex ? { ...c, selected: entry.path } : c));
    location = entry.path; // optimistic; corrected to canonical below
    activeCol = colIndex + 1;
    pendingCol = { afterIndex: colIndex, seq };
    try {
      const listing = await fsList(entry.path, getSetting("files.showHidden"));
      if (seq !== navSeq) return;
      columns = [
        ...columns.slice(0, colIndex + 1),
        {
          dir: listing.path,
          entries: listing.entries,
          truncated: listing.truncated === true,
          selected: null,
        },
      ];
      location = listing.path;
      activeCol = colIndex + 1;
      error = null;
      onNavigate(listing.path);
      await tick();
      revealColumn(colIndex + 1);
    } catch (e) {
      if (seq !== navSeq) return;
      // Couldn't open (permission/gone): drop deeper columns, keep the parent.
      columns = columns.slice(0, colIndex + 1);
      location = columns[colIndex]?.dir ?? location;
      activeCol = colIndex;
      error = message(e);
    } finally {
      if (pendingCol?.seq === seq) pendingCol = null;
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
    if (entry.broken) return; // a dangling symlink opens nothing
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
    // Copy / cut / paste, scoped to the focused Finder so terminals and the
    // composer keep their own Cmd+C/X/V.
    if (e.metaKey || e.ctrlKey) {
      const entry = col.entries[cur];
      if (e.key === "c" && entry !== undefined && !entry.broken) {
        e.preventDefault();
        copyFile(entry.path, entry.kind);
        return;
      }
      if (e.key === "x" && entry !== undefined && !entry.broken) {
        e.preventDefault();
        cutFile(entry.path, entry.kind);
        return;
      }
      if (e.key === "v" && fileClip() !== null) {
        e.preventDefault();
        void pasteInto(col.dir);
        return;
      }
    }
    if (e.key === "Escape" && fileClip() !== null) {
      clearClip();
      return;
    }
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

  // --- inline create/rename + the context menu -------------------------------

  /** The one in-flight inline edit (create at the top of a column, or a
   *  rename replacing an entry row). */
  let edit = $state<
    | { mode: "create"; kind: "file" | "dir"; colIndex: number; dir: string }
    | { mode: "rename"; colIndex: number; path: string; name: string; kind: "dir" | "file" }
    | null
  >(null);
  let editDraft = $state("");
  let editError = $state<string | null>(null);

  function beginCreate(kind: "file" | "dir", colIndex: number, dir: string): void {
    edit = { mode: "create", kind, colIndex, dir };
    editDraft = "";
    editError = null;
  }

  /** Create INSIDE a dir entry: open its column first, then edit there. */
  async function beginCreateInside(kind: "file" | "dir", colIndex: number, entry: FsEntry): Promise<void> {
    await openDir(colIndex, entry);
    beginCreate(kind, colIndex + 1, entry.path);
  }

  function beginRename(colIndex: number, entry: FsEntry): void {
    edit = { mode: "rename", colIndex, path: entry.path, name: entry.name, kind: entry.kind };
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
        const base = cur.dir === "/" ? "" : cur.dir;
        const created = await fsCreateOp(`${base}/${name}`, cur.kind);
        cancelEdit();
        if (cur.kind === "file") onOpenFile(created, false);
        // The fsEpoch rebuild below refreshes the columns.
      } else {
        if (name === cur.name) {
          cancelEdit();
          return;
        }
        const i = cur.path.lastIndexOf("/");
        const parent = i > 0 ? cur.path.slice(0, i) : "";
        await fsRenameOp(cur.path, `${parent}/${name}`);
        cancelEdit();
      }
    } catch (e) {
      editError = message(e);
    }
  }

  function onEditKeydown(e: KeyboardEvent): void {
    e.stopPropagation(); // the cols container owns arrows/Enter otherwise
    if (e.key === "Enter") {
      e.preventDefault();
      void commitEdit();
    } else if (e.key === "Escape") {
      e.preventDefault();
      cancelEdit(); // nulling first makes the following blur a no-op
    }
  }

  async function copyPath(p: string): Promise<void> {
    if (await writeClipboard(p)) return;
    try {
      await navigator.clipboard.writeText(p);
    } catch {
      // clipboard unavailable — quiet
    }
  }

  /** Paste target for a row: into the dir itself, else its parent column. */
  function pasteDirFor(colIndex: number, entry: FsEntry): string {
    return entry.kind === "dir" && !entry.broken ? entry.path : (columns[colIndex]?.dir ?? "");
  }

  function menuFor(colIndex: number, entry: FsEntry): ContextMenuEntry[] {
    const col = columns[colIndex];
    const clip = fileClip();
    // A broken symlink can't be opened, downloaded, or created inside — only
    // its link renamed/copied/deleted (all act on the link itself).
    if (entry.broken) {
      return [
        { label: "Copy", onSelect: () => copyFile(entry.path, entry.kind) },
        { label: "Cut", onSelect: () => cutFile(entry.path, entry.kind) },
        "separator",
        { label: "Rename…", onSelect: () => beginRename(colIndex, entry) },
        { label: "Copy Path", onSelect: () => void copyPath(entry.path) },
        "separator",
        { label: "Delete…", danger: true, onSelect: () => requestDelete(entry.path, entry.kind) },
      ];
    }
    const open: ContextMenuEntry =
      entry.kind === "dir"
        ? { label: "Open", onSelect: () => void openDir(colIndex, entry) }
        : { label: "Open", onSelect: () => openFile(colIndex, entry, false) };
    const createTarget: ContextMenuEntry[] =
      entry.kind === "dir"
        ? [
            { label: "New File…", onSelect: () => void beginCreateInside("file", colIndex, entry) },
            { label: "New Folder…", onSelect: () => void beginCreateInside("dir", colIndex, entry) },
          ]
        : [
            { label: "New File…", onSelect: () => beginCreate("file", colIndex, col.dir) },
            { label: "New Folder…", onSelect: () => beginCreate("dir", colIndex, col.dir) },
          ];
    return [
      open,
      "separator",
      ...createTarget,
      "separator",
      { label: "Copy", onSelect: () => copyFile(entry.path, entry.kind) },
      { label: "Cut", onSelect: () => cutFile(entry.path, entry.kind) },
      {
        label: clip === null ? "Paste" : `Paste ${basename(clip.path)}`,
        disabled: clip === null,
        hint: clip === null ? "nothing copied" : undefined,
        onSelect: () => void pasteInto(pasteDirFor(colIndex, entry)),
      },
      "separator",
      { label: "Rename…", onSelect: () => beginRename(colIndex, entry) },
      "separator",
      ...(remote
        ? [{ label: "Download", onSelect: () => void fsDownload(entry.path) } as ContextMenuEntry]
        : []),
      { label: "Copy Path", onSelect: () => void copyPath(entry.path) },
      "separator",
      {
        label: "Delete…",
        danger: true,
        onSelect: () => requestDelete(entry.path, entry.kind),
      },
    ];
  }

  function columnMenu(colIndex: number): ContextMenuEntry[] {
    const dir = columns[colIndex]?.dir;
    if (dir === undefined) return [];
    const clip = fileClip();
    return [
      { label: "New File…", onSelect: () => beginCreate("file", colIndex, dir) },
      { label: "New Folder…", onSelect: () => beginCreate("dir", colIndex, dir) },
      "separator",
      {
        label: clip === null ? "Paste" : `Paste ${basename(clip.path)}`,
        disabled: clip === null,
        hint: clip === null ? "nothing copied" : undefined,
        onSelect: () => void pasteInto(dir),
      },
    ];
  }

  function parentOf(p: string): string {
    const i = p.lastIndexOf("/");
    return i > 0 ? p.slice(0, i) : "/";
  }

  /** Reveal a column by the smallest possible horizontal movement. A blanket
   *  scroll-to-max used to push the selected folder all the way left on every
   *  refresh, even when the user was inspecting an earlier column. */
  function revealColumn(index: number): void {
    const el = colsEl?.querySelector<HTMLElement>(`.col[data-column-index="${index}"]`);
    el?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }

  // Coalesce refresh bursts and re-list only the columns whose membership or
  // visible file metadata changed. Navigation has its own monotonic sequence,
  // so a stale refresh can never overwrite a newer location.
  const REFRESH_DEBOUNCE_MS = 200;
  let refreshTimer: ReturnType<typeof setTimeout> | null = null;
  let refreshDirs = new Set<string>();
  function scheduleRefresh(dirs: Iterable<string>): void {
    for (const dir of dirs) refreshDirs.add(dir);
    if (refreshTimer !== null) return;
    refreshTimer = setTimeout(() => {
      refreshTimer = null;
      const targets = refreshDirs;
      refreshDirs = new Set();
      void refreshVisible(targets);
    }, REFRESH_DEBOUNCE_MS);
  }

  async function refreshVisible(targets: Set<string>): Promise<void> {
    const seq = navSeq;
    const visible = columns.filter((column) => targets.has(column.dir));
    if (visible.length === 0) return;
    try {
      const listings = await Promise.all(
        visible.map((column) => fsList(column.dir, getSetting("files.showHidden"))),
      );
      if (seq !== navSeq) return;
      const byDir = new Map(listings.map((listing) => [listing.path, listing]));
      columns = columns.map((column) => {
        const listing = byDir.get(column.dir);
        if (listing === undefined) return column;
        const selected = column.selected;
        return {
          dir: listing.path,
          entries: listing.entries,
          truncated: listing.truncated === true,
          selected:
            selected !== null && listing.entries.some((entry) => entry.path === selected)
              ? selected
              : null,
        };
      });
      error = null;
    } catch (e) {
      if (seq === navSeq) error = message(e);
    }
  }

  $effect(() => () => {
    if (refreshTimer !== null) clearTimeout(refreshTimer);
  });

  // Any fs mutation refreshes the directly affected visible folders. When it
  // moved or removed the current location itself, follow it — the App-side tab
  // rewrite then matches and no-ops.
  let lastEpoch = 0;
  $effect(() => {
    const epoch = $fsEpoch;
    const m = $lastFsMutation;
    untrack(() => {
      if (epoch === lastEpoch) return;
      lastEpoch = epoch;
      if (epoch === 0 || location === "") return;
      let target = location;
      if (m?.kind === "rename" && (location === m.from || location.startsWith(`${m.from}/`))) {
        target = m.to + location.slice(m.from.length);
      } else if (m?.kind === "delete" && (location === m.path || location.startsWith(`${m.path}/`))) {
        const i = m.path.lastIndexOf("/");
        target = i > 0 ? m.path.slice(0, i) : "/";
      }
      if (target !== location) {
        void navigateTo(target);
        return;
      }
      const dirs = new Set<string>();
      if (m?.kind === "rename") {
        dirs.add(parentOf(m.from));
        dirs.add(parentOf(m.to));
      } else if (m !== null && m !== undefined) {
        dirs.add(parentOf(m.path));
      }
      scheduleRefresh(dirs);
    });
  });

  // Disk observations have no rename provenance, so rebuild the visible chain
  // in place. A vanished current directory retreats to its parent; App applies
  // the same retarget to the persisted Finder tab.
  let lastDiskSeq = 0;
  $effect(() => {
    const change = $lastDiskChange;
    untrack(() => {
      if (change === null || change.seq === lastDiskSeq || location === "") return;
      lastDiskSeq = change.seq;
      let target = location;
      for (const removed of change.removedDirs) {
        if (target === removed || target.startsWith(`${removed}/`)) {
          const i = removed.lastIndexOf("/");
          target = i > 0 ? removed.slice(0, i) : "/";
        }
      }
      if (target !== location) {
        void navigateTo(target);
        return;
      }
      const dirs = new Set(change.dirs);
      for (const file of [...change.files, ...change.removed]) dirs.add(parentOf(file));
      for (const removed of change.removedDirs) dirs.add(parentOf(removed));
      scheduleRefresh(dirs);
    });
  });

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
      <div
        class="col"
        class:active={ci === activeCol}
        role="group"
        aria-label={col.dir}
        data-finder-dir={col.dir}
        data-column-index={ci}
        oncontextmenu={(e) => contextMenu.openAt(e, columnMenu(ci))}
      >
        {#if edit?.mode === "create" && edit.colIndex === ci}
          <div class="row editing">
            <span class="glyph">
              {#if edit.kind === "dir"}
                <FolderIcon size={14} />
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
            <div class="edit-error">{editError}</div>
          {/if}
        {/if}
        {#if col.entries.length === 0 && !(edit?.mode === "create" && edit.colIndex === ci)}
          <div class="empty">empty</div>
        {/if}
        {#each col.entries as entry (entry.path)}
          {#if edit?.mode === "rename" && edit.path === entry.path}
            <div class="row editing">
              <span class="glyph">
                {#if entry.kind === "dir"}
                  <FolderIcon size={14} />
                {:else}
                  <FileIcon path={entry.path} size={14} />
                {/if}
              </span>
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
            </div>
            {#if editError !== null}
              <div class="edit-error">{editError}</div>
            {/if}
          {:else}
            <button
              class="row"
              class:sel={entry.path === col.selected}
              class:cut={isCutPending(entry.path)}
              title={entry.symlink ? `${entry.path} → ${entry.target ?? ""}${entry.broken ? " (missing)" : ""}` : entry.path}
              onclick={(e) => onRowClick(ci, entry, e)}
              oncontextmenu={(e) => contextMenu.openAt(e, menuFor(ci, entry))}
            >
              <span class="glyph">
                {#if entry.kind === "dir"}
                  <FolderIcon size={14} link={entry.symlink} />
                {:else}
                  <FileIcon path={entry.path} size={14} link={entry.symlink} broken={entry.broken} />
                {/if}
              </span>
              <span class="name" class:symlink={entry.symlink} class:broken={entry.broken}>{entry.name}</span>
              {#if entry.broken}
                <span class="meta broken-meta">broken link</span>
              {:else if entry.kind === "dir"}
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
          {/if}
        {/each}
        {#if col.truncated}
          <div class="listing-limit" role="status">
            Showing the first {col.entries.length.toLocaleString()} entries
          </div>
        {/if}
      </div>
    {/each}
    {#if pendingCol !== null}
      <!-- The incoming column while a descend lists (macOS Finder). Delayed so
           a fast local open never flickers. -->
      <div class="col col-pending" role="group" aria-label="loading">
        <Spinner delay={150} />
      </div>
    {/if}
    {#if loading && columns.length === 0 && error === null}
      <Spinner delay={150} label="listing files…" />
    {:else if columns.length === 0 && error === null && !loading}
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
    position: relative;
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

  /* The incoming-column placeholder while a descend lists: same width, its own
     positioning context so the delayed Spinner centers inside it. */
  .col-pending {
    position: relative;
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
    content-visibility: auto;
    contain-intrinsic-size: auto 27px;
  }

  .listing-limit {
    position: sticky;
    bottom: 0;
    margin: 5px 3px 2px;
    padding: 5px 7px;
    border: 1px solid var(--edge);
    border-radius: 5px;
    background: color-mix(in srgb, var(--pane-bg, var(--bg)) 92%, transparent);
    color: var(--muted);
    font-size: var(--text-xs);
    text-align: center;
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

  /* A symlink name reads one step quieter and italic (the alias convention). */
  .name.symlink {
    font-style: italic;
    color: var(--muted);
  }

  /* A broken symlink is tinted with the error color. */
  .name.broken {
    color: var(--err);
  }

  /* A cut-pending row dims until the paste lands (or Escape clears it). */
  .row.cut {
    opacity: 0.5;
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

  .broken-meta {
    color: var(--err);
    opacity: 0.85;
  }

  .row.editing {
    cursor: default;
  }

  .edit-input {
    flex: 1;
    min-width: 0;
    font-family: var(--mono);
    font-size: var(--text-sm);
    color: var(--fg);
    background: var(--bg);
    border: 1px solid color-mix(in srgb, var(--accent) 45%, var(--edge));
    border-radius: 4px;
    padding: 1px 5px;
    outline: none;
  }

  .edit-input::placeholder {
    color: var(--muted);
    opacity: 0.6;
  }

  .edit-error {
    padding: 1px 6px 3px;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--err);
    white-space: normal;
    word-break: break-word;
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
