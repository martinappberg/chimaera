<script lang="ts">
  /**
   * Lazy directory tree for the rail's FILES section. Each expanded dir is
   * one /fs/list call; listings are cached per path and refreshed every time
   * the dir is re-expanded. The tree renders flat rows (indent by depth) —
   * no recursive components, trivial scrolling.
   */
  import { tick, untrack } from "svelte";
  import { basename, dirLabel, dirname, fsDownload, fsList, type FsEntry } from "../previews/files";
  import { getSetting } from "../settings/store.svelte";
  import { gitIndex, gitStatus, type GitEntry } from "./git";
  import { decoFor, dirColor } from "./gitDeco";
  import { fsCreateOp, fsEpoch, fsRenameOp, lastFsMutation, requestDelete } from "./fsEvents";
  import { clearDiskDirs, lastDiskChange, setDiskDirs } from "./diskWatch";
  import {
    clearClip,
    copyFile,
    cutFile,
    fileClip,
    isCutPending,
    pasteInto,
  } from "./fileClipboard.svelte";
  import { isRemoteHost } from "../net/api";
  import { stemLength, validateEntryName } from "../shared/fsNames";
  import { contextMenu, type ContextMenuEntry } from "../shared/contextMenu.svelte";
  import { writeClipboard } from "../net/native";
  import FileIcon from "../shared/FileIcon.svelte";
  import FolderIcon from "../shared/FolderIcon.svelte";
  import Spinner from "../previews/Spinner.svelte";

  /** Local-daemon windows hide Download (the file already lives here). */
  const remote = isRemoteHost();

  interface Props {
    /** Workspace root on the daemon's filesystem. */
    root: string;
    /** Open a file surface in the layout (single click → a PREVIEW tab). */
    onOpen(path: string): void;
    /** Open a file as a PERMANENT tab (double click, or a just-created file). */
    onOpenPinned(path: string): void;
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
    /**
     * The folder an OS-desktop file drag is hovering: a dir row's path (a
     * file row targets its parent — App reads the row's `data-drop-dir`), or
     * the root for the tree background; null while no file drag is over the
     * tree. The tree lights that exact folder and names it, so the drop
     * destination is never a guess.
     */
    dropDir?: string | null;
    /** The hovered row's tree-position key for the drop (the row's
     *  `data-drop-key`, which App forwards): the same canonical path can sit
     *  under a symlink AND its target, so the highlight resolves by position
     *  when it can, by path only for the fallbacks. */
    dropKey?: string | null;
  }

  let {
    root,
    onOpen,
    onOpenPinned,
    onDragStart,
    activePath,
    reveal = null,
    createRequest = null,
    dropDir = null,
    dropKey = null,
  }: Props = $props();

  /** A row's own left padding (CSS `--row-pad`): the sticky probe lands
   *  inside it, on the row element itself rather than a glyph. */
  const ROW_PAD_PX = 8;
  /** Sticky ancestor cap: a deeper chain shows the nearest three. */
  const STICKY_MAX = 3;

  let expanded = $state<Set<string>>(new Set());
  const diskDirOwner = {};

  // Only dirs whose rows are visible are worth monitoring. A collapsed subtree
  // reloads when expanded, so recursively watching it would buy nothing.
  $effect(() => setDiskDirs(diskDirOwner, [root, ...expanded]));
  $effect(() => () => clearDiskDirs(diskDirOwner));
  let listings = $state<Map<string, FsEntry[]>>(new Map());
  let truncatedDirs = $state<Set<string>>(new Set());
  let loading = $state<Set<string>>(new Set());
  let rootError = $state<string | null>(null);

  interface Row {
    entry: FsEntry;
    depth: number;
    /** Tree-position key, unique even when two rows share a path — a symlinked
     *  dir's children come back with the target's CANONICAL paths, so the same
     *  `entry.path` can appear under both the real dir and the symlink. Keying
     *  the `{#each}` on this (parent-scoped) avoids the duplicate-key breakage
     *  that stranded the "listing…" row and made rows jump. */
    key: string;
    /** Index of the parent dir row (-1 at the root level) — POSITIONAL, so a
     *  symlinked dir's children (canonical paths) still resolve to the row
     *  they sit under. Stamped by the walk; O(1) for every consumer. */
    parent: number;
    /** Last row of this row's visible subtree (itself for a file or a
     *  collapsed dir). */
    end: number;
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
    // `keyPrefix` scopes each row's key by its tree position (see Row.key).
    const walk = (dir: string, depth: number, keyPrefix: string, parent: number): boolean => {
      const entries = listings.get(dir);
      if (entries === undefined) return false;
      let any = false;
      for (const e of entries) {
        const key = `${keyPrefix}/${e.name}`;
        const selfMatch = q === "" || e.name.toLowerCase().includes(q);
        if (e.kind === "dir") {
          // A filtered dir is shown when it (or a loaded descendant) matches;
          // expand into it while filtering even if collapsed, so matches surface.
          const descend = q !== "" || expanded.has(e.path);
          const before = out.length;
          const marker: Row = { entry: e, depth, key, parent, end: before };
          out.push(marker);
          const childMatched = descend ? walk(e.path, depth + 1, key, before) : false;
          if (q !== "" && !selfMatch && !childMatched) {
            out.length = before; // prune a dir with no matches under it
          } else {
            marker.end = out.length - 1;
            any = true;
          }
        } else if (selfMatch) {
          out.push({ entry: e, depth, key, parent, end: out.length });
          any = true;
        }
      }
      return any;
    };
    walk(root, 0, "", -1);
    return out;
  });

  // --- tree-position lookups over the flat `rows` ------------------------------
  // Parent/subtree are POSITIONAL (stamped by the walk above), never
  // path-derived: a symlinked dir's children carry canonical paths, so the
  // path's parent can name a row that is not the one above it.

  /** Index of row `i`'s parent dir row, or -1 at the root level. */
  function parentIndex(i: number): number {
    return rows[i]?.parent ?? -1;
  }

  /** Last row inside dir row `i`'s VISIBLE subtree (`i` itself when none). */
  function subtreeEnd(i: number): number {
    return rows[i].end;
  }

  /** Ancestor dir rows of `i`, root-first. */
  function ancestorsOf(i: number): number[] {
    const out: number[] = [];
    for (let p = parentIndex(i); p >= 0; p = parentIndex(p)) out.unshift(p);
    return out;
  }

  // --- hover cue: light the hovered row's parent guide ------------------------
  // One raw state for the whole tree — a pointer move that stays inside the same
  // parent costs one comparison, and a change costs each row a range check —
  // instead of per-row hover tracking. Rows (start, end] carry the parent's
  // depth as `--hot-level`; the CSS draws that one guide strong.
  let hot = $state.raw<{ start: number; end: number; depth: number } | null>(null);

  function onTreePointerOver(e: PointerEvent): void {
    const node = e.target instanceof Element ? e.target.closest<HTMLElement>(".node[data-index]") : null;
    if (node === null) return; // create/error/listing rows keep the current cue
    const p = parentIndex(Number(node.dataset.index));
    if (p < 0) {
      if (hot !== null) hot = null; // a root-level row is in no folder
      return;
    }
    if (hot?.start === p) return;
    hot = { start: p, end: subtreeEnd(p), depth: rows[p].depth };
  }

  // --- sticky ancestors ("you are here") ---------------------------------------
  // The chain is the ancestors of the first row VISIBLE below the overlay, and
  // the overlay's height is the chain's own — a fixed point found by probing
  // the DOM (elementsFromPoint, plural: the overlay itself sits on top and the
  // inline create/error/listing rows make row math unreliable). It runs once
  // per frame, on scroll and when `rows` change, and never while hidden — no
  // scroll events reach an unseen tree.
  interface StickyRow {
    index: number;
    key: string;
    entry: FsEntry;
    depth: number;
  }

  let sticky = $state.raw<StickyRow[]>([]);
  let scrollEl = $state<HTMLElement | null>(null);
  let stickyFrame = 0;

  function scheduleSticky(): void {
    if (stickyFrame === 0) stickyFrame = requestAnimationFrame(computeSticky);
  }

  /** The tree row element at `y` px below the scroller's top edge (null:
   *  none). A create/error/listing row resolves to the next real row — it
   *  shares that neighbour's ancestry. */
  function rowElementAtY(y: number): HTMLElement | null {
    const s = scrollEl;
    if (s === null) return null;
    const r = s.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) return null;
    for (const el of document.elementsFromPoint(r.left + ROW_PAD_PX + 4, r.top + y)) {
      const node = el.closest<HTMLElement>(".tree .node, .tree .edit-error");
      if (node === null) continue;
      let cur: Element | null = node;
      while (cur !== null && !(cur instanceof HTMLElement && cur.dataset.index !== undefined)) {
        cur = cur.nextElementSibling;
      }
      return cur instanceof HTMLElement ? cur : null;
    }
    return null;
  }

  /** Index of the row at `y` (see rowElementAtY), -1 for none. */
  function rowIndexAtY(y: number): number {
    const el = rowElementAtY(y);
    return el === null ? -1 : Number(el.dataset.index);
  }

  function rowHeight(): number {
    return treeEl?.querySelector<HTMLElement>(".node")?.offsetHeight ?? 23;
  }

  function computeSticky(): void {
    stickyFrame = 0;
    const s = scrollEl;
    if (s === null || rows.length === 0 || s.scrollTop <= 0) {
      if (sticky.length > 0) sticky = [];
      return;
    }
    const h = rowHeight();
    // Iterate overlay height → first visible row → its ancestors. A chain
    // whose bottom row's subtree ends right under the overlay can alternate
    // between two lengths; the shorter one hides fewer real rows, so it wins.
    let k = 0;
    let best: number[] | null = null;
    for (let iter = 0; iter < 4; iter++) {
      const i = rowIndexAtY(k * h + 1);
      const chain = i < 0 ? [] : ancestorsOf(i).slice(-STICKY_MAX);
      if (chain.length === k) {
        best = chain;
        break;
      }
      if (best === null || chain.length < best.length) best = chain;
      k = chain.length;
    }
    const next = (best ?? []).map((i) => ({ index: i, key: rows[i].key, entry: rows[i].entry, depth: rows[i].depth }));
    if (next.length !== sticky.length || next.some((r, j) => r.key !== sticky[j].key)) sticky = next;
  }

  $effect(() => {
    const s = scrollEl;
    if (s === null) return;
    const onScroll = (): void => scheduleSticky();
    s.addEventListener("scroll", onScroll, { passive: true });
    // The shelf is cleared while the tree is unmeasurable (the rail folded
    // to zero width keeps it mounted); a resize brings it back without
    // waiting for the next scroll.
    const ro = new ResizeObserver(scheduleSticky);
    ro.observe(s);
    return () => {
      s.removeEventListener("scroll", onScroll);
      ro.disconnect();
      if (stickyFrame !== 0) {
        cancelAnimationFrame(stickyFrame);
        stickyFrame = 0;
      }
    };
  });

  // --- scroll anchoring across row changes --------------------------------------
  // Rows above the viewport come and go (a relist as an agent writes files,
  // an expand elsewhere); the first visible row must stay where it is.
  // WebKit has no scroll anchoring of its own and the scroller opts out of
  // Chromium's/Firefox's (it fights the collapse anchoring), so the component
  // anchors every row change itself: a pre-DOM snapshot of the first row
  // under the shelf, restored once the new rows are in.
  let anchorKey: string | null = null;
  let anchorTop = 0;

  $effect.pre(() => {
    void rows;
    untrack(() => {
      anchorKey = null;
      const s = scrollEl;
      if (s === null || s.scrollTop <= 0) return;
      const el = rowElementAtY(sticky.length * rowHeight() + 1);
      if (el === null) return;
      anchorKey = el.dataset.key ?? null;
      anchorTop = el.getBoundingClientRect().top;
    });
  });

  function restoreAnchor(): void {
    const key = anchorKey;
    anchorKey = null;
    const s = scrollEl;
    if (key === null || s === null) return;
    const el = treeEl?.querySelector<HTMLElement>(`.node[data-key="${CSS.escape(key)}"]`);
    if (el == null) return; // the anchor row itself went (a collapse: anchorRow takes over)
    const delta = el.getBoundingClientRect().top - anchorTop;
    if (delta !== 0) s.scrollTop += delta;
  }

  // Any row change (expand/collapse, relist, filter) shifts what sits under
  // the overlay and invalidates the hover cue's indices. Reads `rows` only;
  // the writes go to other state, so this cannot re-trigger itself.
  $effect(() => {
    void rows;
    untrack(() => {
      if (hot !== null) hot = null;
      restoreAnchor();
      scheduleSticky();
    });
  });

  /** Scroll so row `el` sits fully in view BELOW its sticky ancestors (which
   *  would otherwise cover a row scrolled to the very top). */
  function ensureRowVisible(el: HTMLElement): void {
    const s = scrollEl;
    if (s === null) return;
    const depth = rows[Number(el.dataset.index ?? -1)]?.depth ?? 0;
    const sr = s.getBoundingClientRect();
    const er = el.getBoundingClientRect();
    const lead = Math.min(depth, STICKY_MAX) * er.height;
    if (er.top < sr.top + lead) {
      s.scrollTop += er.top - (sr.top + lead);
      // The shelf rebuilds from whatever now sits at the top; deep rows above
      // the target can make it taller than the lead, so push once more.
      computeSticky();
      const cover = sticky.length * er.height;
      const top = el.getBoundingClientRect().top;
      if (top < sr.top + cover) s.scrollTop += top - (sr.top + cover);
    } else if (er.bottom > sr.bottom) {
      s.scrollTop += er.bottom - sr.bottom;
    }
  }

  function rowElement(index: number, path: string): HTMLElement | null {
    const byIndex = treeEl?.querySelector<HTMLElement>(`.node[data-index="${index}"]`) ?? null;
    if (byIndex !== null && byIndex.dataset.path === path) return byIndex;
    return treeEl?.querySelector<HTMLElement>(`.node[data-path="${CSS.escape(path)}"]`) ?? null;
  }

  /** A sticky row: its chevron collapses the folder (anchored, see toggle);
   *  the rest of it scrolls to the folder's real row. */
  function onStickyClick(e: MouseEvent, s: StickyRow): void {
    if (e.target instanceof Element && e.target.closest(".chev-hit") !== null) {
      toggle(s.entry.path);
      return;
    }
    const el = rowElement(s.index, s.entry.path);
    if (el !== null) ensureRowVisible(el);
  }

  /** After a collapse removed rows below `path`, keep ITS row in view: a row
   *  above the viewport (collapsed from its sticky copy, or via the keyboard)
   *  lands right under its ancestors; a visible one stays where it is — the
   *  browser only clamps scrollTop, which moves content down, never away. */
  async function anchorRow(path: string): Promise<void> {
    await tick();
    const el = treeEl?.querySelector<HTMLElement>(`.node[data-path="${CSS.escape(path)}"]`);
    if (el != null) ensureRowVisible(el);
  }

  function collapseAll(): void {
    closeFilter(); // a filtered view expands everything it shows
    expanded = new Set();
    if (scrollEl !== null) scrollEl.scrollTop = 0;
  }

  // --- OS-desktop drop target ---------------------------------------------------
  const rootPath = $derived(root.length > 1 && root.endsWith("/") ? root.slice(0, -1) : root);
  const dropIsRoot = $derived(dropDir !== null && (dropDir === root || dropDir === rootPath));
  /** The targeted dir row and its visible subtree (start, end] — the folder's
   *  extent washes so "into this folder" reads as a region, not a line. */
  const dropRange = $derived.by(() => {
    if (dropDir === null || dropIsRoot) return null;
    // By position when the hovered row named itself, by path otherwise.
    let i = dropKey === null ? -1 : rows.findIndex((r) => r.key === dropKey);
    if (i < 0) i = rows.findIndex((r) => r.entry.kind === "dir" && r.entry.path === dropDir);
    return i < 0 ? null : { start: i, end: subtreeEnd(i), depth: rows[i].depth };
  });
  const dropLabel = $derived(dropDir === null ? "" : dirLabel(dropIsRoot ? rootPath : dropDir));

  // A native drag suppresses pointer events, so the hover cue would freeze on
  // whatever was last hovered; clear it so only the drop highlight shows.
  $effect(() => {
    if (dropDir === null) return;
    untrack(() => {
      if (hot !== null) hot = null;
    });
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
      truncatedDirs = new Set();
      rootError = null;
      lastGitEpoch = -1;
      prevGitEntries = new Map();
      relistDirs = new Set();
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

  // A git epoch bump means files may have APPEARED or vanished (an agent wrote a
  // new file, a checkout removed one). Re-list ONLY dirs whose direct listing
  // could have changed, instead of the whole tree. A file that merely stays
  // modified changes no listing (its row is there; its badge updates reactively
  // via gitIndex). Plain `let` (not $state): written inside the effect that
  // reads the epoch.
  interface GitListingSig {
    sig: string;
    paths: string[];
  }

  let lastGitEpoch = -1;
  let prevGitEntries = new Map<string, GitListingSig>();

  function gitListingSig(e: GitEntry): GitListingSig {
    return {
      // Status-code transitions can change filesystem presence (M -> D, D -> M)
      // even when the path stays in the dirty set.
      sig: `${e.x}${e.y}:${e.orig ?? ""}`,
      // A clean-file rename enters the dirty set only at the destination; the
      // source parent must still relist so its stale row disappears.
      paths: e.orig === null ? [e.path] : [e.path, e.orig],
    };
  }

  function addRelistAncestors(dirs: Set<string>, path: string): void {
    const r = rootPath;
    let dir = dirname(path);
    while (true) {
      dirs.add(dir);
      if (dir === r || dir === "/") break;
      if (r !== "/" && !dir.startsWith(`${r}/`)) break;
      dir = dirname(dir);
    }
  }

  $effect(() => {
    const status = $gitStatus;
    const epoch = status?.epoch ?? -1;
    untrack(() => {
      if (epoch < 0 || epoch === lastGitEpoch) return;
      const first = lastGitEpoch < 0;
      lastGitEpoch = epoch;
      const cur = new Map((status?.entries ?? []).map((e) => [e.path, gitListingSig(e)]));
      if (first) {
        prevGitEntries = cur; // the initial fetch's listing is already current
        return;
      }
      const dirs = new Set<string>();
      for (const [path, next] of cur) {
        if (prevGitEntries.get(path)?.sig !== next.sig) {
          for (const p of next.paths) addRelistAncestors(dirs, p);
        }
      }
      for (const [path, prev] of prevGitEntries) {
        if (!cur.has(path)) {
          for (const p of prev.paths) addRelistAncestors(dirs, p);
        }
      }
      prevGitEntries = cur;
      if (dirs.size > 0) scheduleRelist(dirs);
    });
  });

  async function load(dir: string): Promise<void> {
    loading = new Set(loading).add(dir);
    try {
      const listing = await fsList(dir, getSetting("files.showHidden"));
      const next = new Map(listings);
      next.set(dir, listing.entries);
      listings = next;
      const truncated = new Set(truncatedDirs);
      if (listing.truncated === true) truncated.add(dir);
      else truncated.delete(dir);
      truncatedDirs = truncated;
      if (dir === root) rootError = null;
    } catch (e) {
      const truncated = new Set(truncatedDirs);
      truncated.delete(dir);
      truncatedDirs = truncated;
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

  // Coalesce targeted re-list requests. A working agent bumps the git/fs epoch
  // on every file it writes; each bump contributes only the DIRS that changed
  // (see the epoch effects), which accumulate here and flush in ONE debounced
  // pass — so a write-burst re-lists just the touched folders, not every
  // expanded dir, and the git+fs double-fire for an in-repo mutation collapses
  // into the same pass. Only currently-visible dirs (root or expanded) list.
  const RELIST_DEBOUNCE_MS = 250;
  let relistTimer: ReturnType<typeof setTimeout> | null = null;
  let relistDirs = new Set<string>();
  function scheduleRelist(dirs: Iterable<string>): void {
    for (const d of dirs) relistDirs.add(d);
    if (relistTimer !== null) return; // a pass is already pending; fold into it
    relistTimer = setTimeout(() => {
      relistTimer = null;
      const targets = relistDirs;
      relistDirs = new Set();
      for (const dir of targets) {
        if (dir === root || expanded.has(dir)) void load(dir);
      }
    }, RELIST_DEBOUNCE_MS);
  }
  $effect(() => () => {
    if (relistTimer !== null) clearTimeout(relistTimer);
  });

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
    const r = rootPath;
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
    const isDir = listings.get(dirname(target))?.some((e) => e.path === target && e.kind === "dir");
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
    const el = treeEl?.querySelector<HTMLElement>(`.node[data-path="${CSS.escape(path)}"]`);
    if (el != null) ensureRowVisible(el);
    setTimeout(() => {
      if (flashPath === path) flashPath = null;
    }, 1200);
  }

  function toggle(dir: string): void {
    const next = new Set(expanded);
    if (next.has(dir)) {
      next.delete(dir);
      expanded = next;
      void anchorRow(dir); // the viewport must not jump to unrelated content
    } else {
      next.add(dir);
      expanded = next;
      void load(dir); // fresh listing on every expand
    }
  }

  /** Paste target for an entry: into the dir itself, else its parent. */
  function pasteDirFor(entry: FsEntry): string {
    return entry.kind === "dir" && !entry.broken ? entry.path : dirname(entry.path);
  }

  function onRowKey(e: KeyboardEvent, entry: FsEntry): void {
    if (edit?.mode === "rename" && edit.path === entry.path) return; // the input owns keys
    // Copy / cut / paste, scoped to the focused tree row.
    if (e.metaKey || e.ctrlKey) {
      if (e.key === "c" && !entry.broken) {
        e.preventDefault();
        copyFile(entry.path, entry.kind);
        return;
      }
      if (e.key === "x" && !entry.broken) {
        e.preventDefault();
        cutFile(entry.path, entry.kind);
        return;
      }
      if (e.key === "v" && fileClip() !== null) {
        e.preventDefault();
        void pasteInto(pasteDirFor(entry));
        return;
      }
    }
    if (e.key === "Escape" && fileClip() !== null) {
      clearClip();
      return;
    }
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      if (entry.broken) return; // a dangling symlink opens nothing
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
        // A just-created file is for editing → open it as a permanent tab.
        if (cur.kind === "file") onOpenPinned(created);
      } else {
        if (name === cur.name) {
          cancelEdit();
          return;
        }
        const parent = dirname(cur.path);
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
    const clip = fileClip();
    // A broken symlink can only be renamed/copied/deleted (all on the link).
    if (entry.broken) {
      return [
        { label: "Copy", onSelect: () => copyFile(entry.path, entry.kind) },
        { label: "Cut", onSelect: () => cutFile(entry.path, entry.kind) },
        "separator",
        { label: "Rename…", onSelect: () => beginRename(entry) },
        { label: "Copy Path", onSelect: () => void copyPath(entry.path) },
        "separator",
        { label: "Delete…", danger: true, onSelect: () => requestDelete(entry.path, entry.kind) },
      ];
    }
    const dirTarget = entry.kind === "dir" ? entry.path : dirname(entry.path);
    return [
      { label: "New File…", onSelect: () => beginCreate("file", dirTarget) },
      { label: "New Folder…", onSelect: () => beginCreate("dir", dirTarget) },
      "separator",
      { label: "Copy", onSelect: () => copyFile(entry.path, entry.kind) },
      { label: "Cut", onSelect: () => cutFile(entry.path, entry.kind) },
      {
        label: clip === null ? "Paste" : `Paste ${basename(clip.path)}`,
        disabled: clip === null,
        hint: clip === null ? "nothing copied" : undefined,
        onSelect: () => void pasteInto(pasteDirFor(entry)),
      },
      "separator",
      { label: "Rename…", onSelect: () => beginRename(entry) },
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

  // Header-button create requests target the workspace root.
  $effect(() => {
    const req = createRequest;
    if (req == null) return;
    void req.nonce; // track repeats
    untrack(() => beginCreate(req.kind, root));
  });

  // Any fs mutation (this tree, the Finder, a tab rename) re-lists the affected
  // dir(s): the mutation names the exact path, so we relist its parent — both
  // parents for a rename/move — rather than the whole tree. Out-of-band paths
  // outside Git arrive through the disk-watch effect below.
  let lastFsEpoch = 0;
  $effect(() => {
    const epoch = $fsEpoch;
    const m = $lastFsMutation;
    untrack(() => {
      if (epoch === lastFsEpoch) return;
      lastFsEpoch = epoch;
      if (epoch === 0) return;
      const dirs = new Set<string>();
      if (m?.kind === "rename") {
        dirs.add(dirname(m.from));
        dirs.add(dirname(m.to));
      } else if (m?.kind === "create" || m?.kind === "delete") {
        dirs.add(dirname(m.path));
      } else {
        dirs.add(root); // no precise path — fall back to the root listing
      }
      scheduleRelist(dirs);
    });
  });

  // Out-of-band creates/deletes/renames arrive as directory invalidations from
  // the daemon's mounted-path monitor (including ignored and non-Git paths).
  let lastDiskSeq = 0;
  $effect(() => {
    const change = $lastDiskChange;
    untrack(() => {
      if (change === null || change.seq === lastDiskSeq) return;
      lastDiskSeq = change.seq;
      const dirs = new Set(change.dirs);
      for (const path of change.removed) dirs.add(dirname(path));
      for (const path of change.removedDirs) {
        dirs.add(path);
        dirs.add(dirname(path));
      }
      scheduleRelist(dirs);
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
      <button
        class="filter-toggle"
        aria-label="collapse folders"
        title="collapse folders"
        disabled={expanded.size === 0}
        onclick={collapseAll}
      >
        <svg viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
          <path
            d="M4 7l4-3.5L12 7M4 12.5L8 9l4 3.5"
            fill="none"
            stroke="currentColor"
            stroke-width="1.4"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
        </svg>
      </button>
      <button class="filter-toggle" aria-label="filter files" title="filter files" onclick={() => openFilter()}>
        <svg viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
          <circle cx="7" cy="7" r="4" fill="none" stroke="currentColor" stroke-width="1.4" />
          <line x1="10" y1="10" x2="13.5" y2="13.5" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" />
        </svg>
      </button>
    {/if}
  </div>
  <!-- The tree is its own scroller so the ancestor overlay can stick to ITS
       top edge (the rail body around it does not scroll). -->
  <div class="tree-scroll" bind:this={scrollEl}>
    <!-- Zero-height sticky anchor: what it holds is painted over the top rows
         and never moves the content. During an OS file drag it names the drop
         destination instead of the ancestors — one message at a time. -->
    <div class="tree-overlay">
      {#if sticky.length > 0}
        <div class="sticky-rows">
          {#each sticky as s (s.key)}
            <!-- A pinned ancestor is a drop target like its real row
                 (data-drop-dir; the shelf stays up during a drag). -->
            <button
              type="button"
              class="sticky-node"
              class:drop-target={dropDir === s.entry.path}
              tabindex="-1"
              title={s.entry.path}
              data-drop-dir={s.entry.path}
              data-drop-key={s.key}
              style:--depth={s.depth}
              onclick={(e) => onStickyClick(e, s)}
            >
              <span class="chev-hit" title="collapse">
                <svg class="chev open" viewBox="0 0 16 16" width="9" height="9" aria-hidden="true">
                  <path
                    d="M6 4l4 4-4 4"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1.6"
                    stroke-linecap="round"
                    stroke-linejoin="round"
                  />
                </svg>
              </span>
              <span class="row-glyph"><FolderIcon open={true} size={14} link={s.entry.symlink} /></span>
              <span class="node-name dir" class:symlink={s.entry.symlink}>{s.entry.name}</span>
            </button>
          {/each}
        </div>
      {/if}
      {#if dropDir !== null}
        <!-- Names the destination, pinned just under the shelf so the place
             and the name read together. -->
        <div class="drop-chip folder tree-drop-label" style:--shelf-rows={sticky.length} role="status">
          upload into <b>{dropLabel}</b>
        </div>
      {/if}
    </div>
  <div
    class="tree"
    class:drop-root={dropIsRoot}
    style:--shelf-rows={sticky.length}
    role="tree"
    tabindex="-1"
    bind:this={treeEl}
    data-tree-root={root}
    onkeydown={onTreeKeydown}
    onpointerover={onTreePointerOver}
    onpointerleave={() => {
      if (hot !== null) hot = null;
    }}
    oncontextmenu={(e) =>
      contextMenu.openAt(e, [
        { label: "New File…", onSelect: () => beginCreate("file", root) },
        { label: "New Folder…", onSelect: () => beginCreate("dir", root) },
        "separator",
        {
          label: fileClip() === null ? "Paste" : `Paste ${basename(fileClip()!.path)}`,
          disabled: fileClip() === null,
          hint: fileClip() === null ? "nothing copied" : undefined,
          onSelect: () => void pasteInto(root),
        },
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
      <div class="node edit-node" style:--depth={depth}>
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
        <div class="edit-error" style:--depth={depth}>{editError}</div>
      {/if}
    {/if}
  {/snippet}
  {#if edit?.mode === "create" && createAfterIndex === -1}
    {@render createRow(0)}
  {/if}
  {#each rows as { entry, depth, key, parent }, i (key)}
    {@const gEntry = entry.kind === "file" ? $gitIndex.files.get(entry.path) : undefined}
    {@const gDeco = gEntry ? decoFor(gEntry) : null}
    {@const gDir = entry.kind === "dir" ? $gitIndex.dirs.get(entry.path) : undefined}
    {@const inHot = hot !== null && i > hot.start && i <= hot.end}
    {@const inDrop = dropRange !== null && i > dropRange.start && i <= dropRange.end}
    <div
      class="node"
      class:active={entry.path === activePath}
      class:flash={entry.path === flashPath}
      class:cut={isCutPending(entry.path)}
      class:guide-hot={inHot}
      class:drop-target={dropRange?.start === i}
      class:drop-within={inDrop}
      role="treeitem"
      aria-expanded={entry.kind === "dir" ? expanded.has(entry.path) : undefined}
      aria-selected={entry.path === activePath}
      tabindex="0"
      title={entry.symlink ? `${entry.path} → ${entry.target ?? ""}${entry.broken ? " (missing)" : ""}` : entry.path}
      data-path={entry.path}
      data-index={i}
      data-key={key}
      data-drop-dir={entry.broken
        ? undefined
        : entry.kind === "dir"
          ? entry.path
          : parent < 0
            ? root
            : rows[parent].entry.path}
      data-drop-key={entry.broken
        ? undefined
        : entry.kind === "dir"
          ? key
          : parent < 0
            ? undefined
            : rows[parent].key}
      style:--depth={depth}
      style:--hot-level={inDrop ? dropRange?.depth : inHot ? hot?.depth : undefined}
      onpointerdowncapture={(e) => {
        // The rename input stays a plain interactive target (rail-row idiom).
        if (e.target instanceof Element && e.target.closest(".edit-input")) return;
        // Both kinds click via the drag's sub-threshold path — a DOM onclick
        // would ALSO fire after a completed drag (pointer capture retargets
        // the click back to this row) and double-act. Skip while renaming.
        onDragStart(e, entry.path, entry.kind, () => {
          if (edit?.mode === "rename" && edit.path === entry.path) return;
          if (entry.broken) return; // a dangling symlink opens nothing
          if (entry.kind === "dir") toggle(entry.path);
          else onOpen(entry.path);
        });
      }}
      ondblclick={() => {
        // VS Code: double-click a file PINS it (the two single-clicks already
        // opened it as a preview; this promotes). Dirs are unaffected.
        if (entry.kind === "file" && !entry.broken && !(edit?.mode === "rename" && edit.path === entry.path)) {
          onOpenPinned(entry.path);
        }
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
          <FolderIcon open={expanded.has(entry.path)} size={14} link={entry.symlink} />
        {:else}
          <FileIcon path={entry.path} size={14} link={entry.symlink} broken={entry.broken} />
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
          class:symlink={entry.symlink}
          class:broken={entry.broken}
          style:color={entry.broken ? undefined : gDeco ? gDeco.color : undefined}>{entry.name}</span>
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
    {#if entry.kind === "dir" && expanded.has(entry.path) && loading.has(entry.path) && listings.get(entry.path) === undefined}
      <!-- First listing of a freshly-expanded dir is in flight: a delayed
           "listing…" row (pure CSS delay, no per-row timer) so a slow remote
           expand shows progress while a fast local one never flickers. A
           re-list of an already-listed dir keeps its stale rows, no spinner. -->
      <div class="node loading-row" style:--depth={depth + 1} role="presentation">
        <span class="chev-spacer" aria-hidden="true"></span>
        <span class="mini-spinner" aria-hidden="true"></span>
        <span class="loading-label">listing…</span>
      </div>
    {/if}
    {#if edit?.mode === "rename" && edit.path === entry.path && editError !== null}
      <div class="edit-error" style:--depth={depth}>{editError}</div>
    {/if}
    {#if edit?.mode === "create" && createAfterIndex === i}
      {@render createRow(depth + 1)}
    {/if}
  {/each}
  {#if truncatedDirs.size > 0}
    <div class="tree-limit" role="status" title={[...truncatedDirs].join("\n")}>
      Some large folders are partially shown
    </div>
  {/if}
  </div>
  </div>
</div>

<style>
  .tree-wrap {
    display: flex;
    flex-direction: column;
    min-height: 0;
    flex: 1; /* fill .files-body so the tree's empty area is right-clickable */
    /* Row geometry, shared by real rows, the sticky copies and the JS probe
       (ROW_PAD_PX in the script mirrors --row-pad). */
    --row-h: calc(var(--text-sm) + 10px);
    --indent: 13px;
    --row-pad: 8px;
    /* Indent guides: one per ancestor level, under that level's chevron
       column. Quiet by default; the hovered row's parent (or the drop
       target) draws its guide strong. */
    --guide: color-mix(in srgb, var(--muted) 28%, transparent);
    --guide-hot: color-mix(in srgb, var(--muted) 75%, transparent);
  }

  /* The scroller. overscroll-behavior keeps a wheel that runs out of tree
     from dragging the rail; the overlay below sticks to this box's top.
     overflow-anchor: the component anchors collapses itself (anchorRow);
     Chrome's own scroll anchoring then re-anchors on a removed child row
     and lands the folder ~4 rows low, and snaps the viewport back on the
     next expand — measured, deterministic. */
  .tree-scroll {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    overflow-y: auto;
    overflow-x: hidden;
    overscroll-behavior: contain;
    overflow-anchor: none;
  }

  .tree-overlay {
    position: sticky;
    top: 0;
    z-index: 2;
    flex: none;
    height: 0;
    overflow: visible;
  }

  .sticky-rows,
  .tree-drop-label {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
  }

  /* Ancestor rows pinned over the scrolled content: painted like real rows
     on the rail's ground, with a hairline + soft shade below so they read as
     a shelf, not as rows that happen to sit at the top. */
  .sticky-rows {
    display: flex;
    flex-direction: column;
    padding: 0 0.45rem;
    background: var(--rail-bg);
    box-shadow:
      0 1px 0 var(--edge),
      0 3px 8px -2px color-mix(in srgb, var(--fg) 14%, transparent);
  }

  /* A <button> so it is clickable without a11y ceremony; strip the UA face
     (grey background, 1px vertical padding) so it paints exactly like a row. */
  .sticky-node {
    appearance: none;
    border: none;
    background-color: transparent;
    padding-top: 0;
    padding-bottom: 0;
    width: 100%;
    font: inherit;
    text-align: left;
    color: inherit;
  }

  /* The sticky chevron is the only part that collapses — give it a real
     target (the row itself scrolls to the folder). */
  .chev-hit {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 9px;
    height: 9px;
    padding: 4px;
    margin: -4px;
    box-sizing: content-box;
    border-radius: 3px;
  }

  .chev-hit:hover {
    background: color-mix(in srgb, var(--fg) 10%, transparent);
  }

  .chev-hit:hover .chev {
    opacity: 1;
    color: var(--fg);
  }

  /* Names the drop destination while an OS file drag is over the tree (the
     global .drop-chip look): pinned just under the ancestor shelf, so it
     stays in view when the drag auto-scrolls the tree. */
  .tree-drop-label {
    --chip-ground: var(--rail-bg);
    top: calc(var(--shelf-rows, 0) * var(--row-h) + 2px);
    margin: 0 0.45rem;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .tree-limit {
    position: sticky;
    bottom: 0;
    margin: 6px 8px;
    padding: 5px 7px;
    border: 1px solid var(--edge);
    border-radius: 5px;
    background: color-mix(in srgb, var(--rail-bg) 88%, transparent);
    backdrop-filter: blur(6px);
    color: var(--muted);
    font-size: var(--text-xs);
    text-align: center;
  }

  /* Quiet tree tools flush-right: collapse-all + the magnifier that expands
     into the filter input. Neither competes with the tree. */
  .filter-bar {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: 2px;
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

  .filter-toggle:disabled {
    opacity: 0.3;
    color: var(--muted);
    background: none;
    cursor: default;
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
    font-size: var(--text-sm);
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
    /* At least the scroller's height (a right-click BELOW the last row still
       hits the tree's context menu — Paste into the root), growing with its
       rows so the bottom banner has a full-height box to stick within. */
    flex: 1 0 auto;
    min-height: 0;
  }

  /* An OS-desktop file drag targets the ROOT (tree background, a root-level
     file): a quiet accent frame marks the whole tree as the drop zone. A
     nested folder lights its own row + subtree instead (.drop-target/-within). */
  .tree.drop-root {
    box-shadow: inset 0 0 0 1.5px color-mix(in srgb, var(--accent) 55%, transparent);
    border-radius: 6px;
    background: color-mix(in srgb, var(--accent) 6%, transparent);
  }

  .tree-error,
  .tree-empty {
    padding: 0.3rem 0.45rem;
    font-size: var(--text-sm);
    color: var(--muted);
    line-height: 1.4;
  }

  .node,
  .sticky-node {
    display: flex;
    align-items: center;
    gap: 4px;
    height: var(--row-h);
    padding-left: calc(var(--row-pad) + var(--depth, 0) * var(--indent));
    padding-right: 0.45rem;
    border-radius: 4px;
    cursor: pointer;
    user-select: none;
    min-width: 0;
    outline: none;
    /* Indent guides with no extra DOM: a 1px line per 13px tile, the image
       sized to exactly `depth` tiles so a root row draws none. The line sits
       at 12–13px of each tile — the centre of that level's 9px chevron
       (which starts at 8px). Rows paint their state via background-COLOR
       only, so the guides survive hover/active/flash. */
    background-image: repeating-linear-gradient(
      to right,
      transparent 0 12px,
      var(--guide) 12px 13px
    );
    background-size: calc(var(--depth, 0) * var(--indent)) 100%;
    background-repeat: no-repeat;
    background-position: 0 0;
  }

  /* The hovered row's parent guide (or the drop target's) drawn strong: a
     second 1px layer at that level, over the quiet set. */
  .node.guide-hot,
  .node.drop-within {
    background-image:
      linear-gradient(var(--guide-hot), var(--guide-hot)),
      repeating-linear-gradient(to right, transparent 0 12px, var(--guide) 12px 13px);
    background-size:
      1px 100%,
      calc(var(--depth, 0) * var(--indent)) 100%;
    background-position:
      calc(12px + var(--hot-level, 0) * var(--indent)) 0,
      0 0;
  }

  /* Keyboard focus and scrollIntoView land BELOW the pinned ancestors
     (--shelf-rows on .tree), never under them. */
  .node {
    scroll-margin-top: calc(var(--shelf-rows, 0) * var(--row-h));
  }

  .node:hover,
  .sticky-node:hover {
    background-color: var(--row-hover);
  }

  .node:focus-visible {
    background-color: var(--row-hover);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--accent) 45%, transparent);
  }

  .node.active {
    background-color: var(--row-active);
  }

  /* Reveal flash (terminal dir links): a brief accent wash that fades. */
  .node.flash {
    background-color: color-mix(in srgb, var(--accent) 18%, transparent);
    transition: background-color 0.9s ease;
  }

  /* OS-desktop drop onto a nested folder: the target row gets a ring + wash,
     and its expanded descendants a light wash with the guide in accent —
     the folder's whole extent reads as the destination. */
  .node.drop-within {
    --guide-hot: color-mix(in srgb, var(--accent) 80%, transparent);
    background-color: color-mix(in srgb, var(--accent) 8%, transparent);
    border-radius: 0;
  }

  .node.drop-target,
  .sticky-node.drop-target {
    background-color: color-mix(in srgb, var(--accent) 20%, transparent);
    box-shadow: inset 0 0 0 1.5px color-mix(in srgb, var(--accent) 75%, transparent);
  }

  .node.drop-target .node-name {
    color: var(--fg);
  }

  .chev {
    flex: none;
    color: var(--muted);
    opacity: 0.65;
    transition:
      transform 0.1s ease,
      opacity 0.12s ease;
  }

  .node:hover .chev,
  .sticky-node .chev {
    opacity: 1;
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
    font-size: var(--text-sm);
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
    line-height: 1.3;
  }

  /* Inline create/rename input, sized like the name it replaces. */
  .edit-input {
    flex: 1;
    min-width: 0;
    font-family: var(--mono);
    font-size: var(--text-sm);
    color: var(--fg);
    background: var(--bg);
    border: 1px solid color-mix(in srgb, var(--accent) 45%, var(--edge));
    border-radius: 3px;
    padding: 0 4px;
    height: calc(var(--text-sm) + 5px);
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
    font-size: var(--text-xs);
    color: var(--err);
    padding-top: 1px;
    padding-bottom: 2px;
    padding-left: calc(var(--row-pad) + var(--depth, 0) * var(--indent));
    white-space: normal;
    word-break: break-word;
  }

  .node-name.dir {
    color: var(--fg);
  }

  /* A symlink reads italic (alias convention); it keeps its dir/file color. */
  .node-name.symlink {
    font-style: italic;
  }

  /* A broken symlink is tinted with the error color. */
  .node-name.broken {
    color: var(--err);
  }

  .node.active .node-name {
    color: var(--fg);
  }

  /* A cut-pending row dims until the paste lands (or Escape clears it). */
  .node.cut {
    opacity: 0.5;
  }

  /* Per-node "listing…" row: a small spinner + label, delayed in via CSS so a
     fast local expand never flickers. No handlers, no tab stop. */
  .loading-row {
    cursor: default;
    color: var(--muted);
    opacity: 0;
    animation: node-load-fade 0.15s ease 0.15s forwards;
    pointer-events: none;
  }

  .mini-spinner {
    flex: none;
    width: 10px;
    height: 10px;
    border-radius: 50%;
    border: 1.5px solid color-mix(in srgb, var(--accent) 30%, transparent);
    border-top-color: var(--accent);
    animation: node-spin 0.7s linear infinite;
  }

  .loading-label {
    font-family: var(--mono);
    font-size: var(--text-sm);
    color: var(--muted);
  }

  @keyframes node-spin {
    to {
      transform: rotate(360deg);
    }
  }

  @keyframes node-load-fade {
    to {
      opacity: 1;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .mini-spinner {
      animation-duration: 1.4s;
    }
    /* Keep it VISIBLE under reduced motion (don't animate it away). */
    .loading-row {
      animation: none;
      opacity: 1;
    }
  }

  /* Git status: a single-letter badge (files) or a rollup dot (collapsed dirs),
     pushed to the row's right edge — quiet, only present when state matters. */
  .git-badge {
    flex: none;
    margin-left: auto;
    font-family: var(--mono);
    font-size: var(--text-xs);
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
