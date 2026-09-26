<script lang="ts">
  /**
   * The paged table grid shared by delimited text (CSV/TSV and the
   * bioinformatics presets in `files.ts`) and spreadsheets (`XlsxView`).
   * The DOM holds only the rows in view plus overscan (`tableGrid.ts`); the
   * loaded window pages in both directions as you scroll and is capped, so a
   * deep jump or a long scroll never grows without bound. Jump to any row
   * from the footer field (the daemon's row index makes that a seek);
   * double-click a cell to read and copy all of it.
   */
  import { tick, untrack } from "svelte";
  import { fsTable, type TablePage } from "./files";
  import { retain, release, type FileEntry } from "./fileStore.svelte";
  import { getSetting } from "../settings/store.svelte";
  import { copyText } from "../shared/clipboard";
  import { activeSelection, clearSelection, setSelection, type FileSelection } from "../shared/reference";
  import { revealRequest, takeReveal, type Reveal } from "../shared/reveal";
  import { tableFragment, tableLabel, tsvQuote, TSV_LIMITS, type TableBlock } from "../shared/locator";
  import ReferenceChip from "../shared/ReferenceChip.svelte";
  import Spinner from "./Spinner.svelte";
  import {
    autoColumnWidths,
    formatRowCount,
    jumpOffset,
    parseRowNumber,
    rowAt,
    virtualWindow,
  } from "./tableGrid";

  interface Props {
    path: string;
    /** Override the page source. Default (CSV/TSV) reads the shared store's
     *  cached first page + `fsTable` for more. A caller that supplies this (the
     *  xlsx viewer, per sheet) bypasses the store and drives the grid directly —
     *  same shape, so selection/resize/paging are unchanged. Pass a STABLE
     *  reference (e.g. a `$derived`); a fresh inline closure each render churns
     *  this component's effect. */
    fetchPage?: (offset: number, limit: number) => Promise<TablePage>;
    /** How a selected block is addressed for an agent: the fragment and
     *  its words. Default: RFC 7111 syntax on 1-based data rows (CSV/TSV);
     *  the xlsx viewer passes its sheet + A1 form. Pass a STABLE reference. */
    locate?: (b: TableBlock) => { fragment: string; label: string };
    /** A block to land on, handed down by a host that takes its reveals
     *  itself (xlsx: the sheet comes first). 1-based data rows/columns;
     *  `nonce` re-triggers an identical one. Without a host, the grid takes
     *  `#row=`/`#col=`/`#cell=` reveals for `path` from the reveal store. */
    reveal?: { table: NonNullable<Reveal["table"]>; nonce: number } | null;
  }

  let { path, fetchPage = undefined, locate = undefined, reveal = null }: Props = $props();

  /** Rows per fetched page (settings ground truth, read per request). */
  const pageRows = () => getSetting("files.tableRowsPerPage");
  /** Fetch the next (or previous) page when the rendered rows come this close
   *  to the loaded window's edge. */
  const PREFETCH_ROWS = 150;
  const OVERSCAN = 12;
  /** The loaded window's ceiling; paging past it drops rows off the far end. */
  const MAX_LOADED_ROWS = 20_000;
  /** A deep jump may need several budgeted daemon scans while the row index
   *  is built; each one must make progress, and this bounds the whole walk. */
  const MAX_SCAN_ROUNDS = 64;
  const MIN_COL_PX = 48;
  const CELL_PAD_PX = 24;
  const AUTO_MAX_CHARS = 40;
  const AUTOFIT_MAX_CHARS = 80;
  const FLASH_MS = 1400;
  /** Until the grid font is measured: close to every mono face at 12-13px. */
  const FALLBACK_CHAR_W = 7.5;

  let columns = $state.raw<string[]>([]);
  let rows = $state.raw<string[][]>([]);
  /** Row number of `rows[0]` in the file (0-based). */
  let loadedOffset = $state(0);
  /** Exact data-row count once known; else the daemon's estimate. */
  let total = $state<number | null>(null);
  let estimate = $state<number | null>(null);
  let atEnd = $state(false);
  let error = $state<string | null>(null);
  let loading = $state(false);
  /** A jump in progress: what it is doing ("indexing… row 400,000"). */
  let seeking = $state<string | null>(null);
  let scroller = $state<HTMLDivElement | null>(null);
  let head = $state<HTMLTableSectionElement | null>(null);
  let scrollTop = $state(0);
  let viewportH = $state(0);
  let rowH = $state(24);
  /** Monospace advance of the grid font (0 = not measured yet); column
   *  widths are counted in it. */
  let charW = $state(0);

  /** Column widths (px); auto-sized from the first page, then the user's. */
  let widths = $state.raw<number[]>([]);
  const userSized = new Set<number>();
  /** Columns detected as numeric get right-aligned tabular figures. */
  let numericCols = $state<Set<number>>(new Set());

  /** Selection: a rectangular block of cells, or whole rows via the gutter.
   *  Rows are file row numbers, so paging the window keeps it anchored. */
  interface Sel {
    r0: number;
    c0: number;
    r1: number;
    c1: number;
  }
  let sel = $state<Sel | null>(null);
  let anchor: { r: number; c: number } | null = null;
  /** A drag is in progress: the block is published once it settles. */
  let selecting = $state(false);
  /** The drag left its first cell: no text selection while it lasts. */
  let blockDrag = $state(false);
  /** A revealed block (`#cell=5,2-9,4`): outlined until the next press. */
  let hit = $state<Sel | null>(null);

  /** Rows flashing after a jump (file row numbers, inclusive). */
  let flashRows = $state<{ from: number; to: number } | null>(null);
  let flashTimer: ReturnType<typeof setTimeout> | null = null;
  let jumpGen = 0;
  /** The footer row field's text while typing (null = follow the scroll). */
  let rowDraft = $state<string | null>(null);

  /** The cell open in the expand popover. */
  interface Expanded {
    r: number;
    c: number;
    column: string;
    text: string;
    left: number;
    top: number;
  }
  let expanded = $state<Expanded | null>(null);
  let copied = $state(false);
  let root = $state<HTMLDivElement | null>(null);

  const win = $derived(virtualWindow(scrollTop, viewportH, rowH, rows.length, OVERSCAN));
  const visibleRows = $derived(rows.slice(win.start, win.end));
  const topRow = $derived(loadedOffset + rowAt(scrollTop, rowH, rows.length) + 1);
  const gutterW = $derived(
    Math.ceil(
      Math.max(5, (loadedOffset + rows.length).toLocaleString("en-US").length + 1) * (charW || FALLBACK_CHAR_W),
    ) + CELL_PAD_PX,
  );
  const tableW = $derived(gutterW + widths.reduce((a, b) => a + b, 0));

  // The shared store entry holds the FIRST page (cached across tab switches,
  // re-fetched in place when the file changes on disk). Further pages load
  // as the window scrolls and are not cached.
  let entry = $state<FileEntry | null>(null);
  $effect(() => {
    const p = path;
    // reset everything for the new file
    columns = [];
    rows = [];
    total = null;
    estimate = null;
    loadedOffset = 0;
    error = null;
    atEnd = false;
    widths = [];
    userSized.clear();
    numericCols = new Set();
    sel = null;
    anchor = null;
    expanded = null;
    loading = true;
    jumpGen += 1;
    seeking = null;
    if (fetchPage !== undefined) {
      // Store-bypass source (xlsx): no shared store entry (so no instant cache /
      // live-on-disk refresh) — the caller remounts us per sheet, which resets
      // everything above, then we fetch that sheet's first page directly.
      entry = null;
      const fp = fetchPage;
      void (async () => {
        try {
          const page = await fp(0, pageRows());
          if (p !== path) return;
          apply(page, "replace");
        } catch (e) {
          if (p !== path) return;
          error = e instanceof Error ? e.message : "failed to load the sheet";
        } finally {
          if (p === path) loading = false;
        }
      })();
      return;
    }
    const e = retain(p);
    entry = e;
    void e.ensureTable();
    return () => release(p);
  });

  // Apply the store's first page whenever it (re)loads — initial fetch, an
  // instant cache hit on return, or a live refresh after a disk change (which
  // resets to the fresh first page, dropping any paged-in window). Only the
  // entry's payload is a dependency: `apply` reads the grid's own state.
  $effect(() => {
    const e = entry;
    if (e === null) return;
    const page = e.table;
    const failure = e.tableError;
    untrack(() => {
      if (page !== null) {
        apply(page, "replace");
        loading = false;
        if (scroller !== null) scroller.scrollTop = 0;
        scrollTop = 0;
      } else if (failure !== null) {
        error = failure;
        loading = false;
      }
    });
  });

  // Viewport height and the row height (the latter follows the text-size
  // setting, which resizes the header row).
  $effect(() => {
    const el = scroller;
    const th = head;
    if (el === null) return;
    const ro = new ResizeObserver(() => {
      viewportH = el.clientHeight;
      measure();
    });
    ro.observe(el);
    if (th !== null) ro.observe(th);
    viewportH = el.clientHeight;
    return () => ro.disconnect();
  });

  // Measure once the first rows render (the grid mounts with the data).
  $effect(() => {
    if (scroller === null || rows.length === 0) return;
    void tick().then(measure);
  });

  $effect(
    () => () => {
      if (flashTimer !== null) clearTimeout(flashTimer);
    },
  );

  /** Read the real row height and font advance from the rendered grid. */
  function measure(): void {
    const el = scroller;
    if (el === null) return;
    const tr = el.querySelector<HTMLElement>("tbody tr.row");
    if (tr !== null) {
      const h = tr.getBoundingClientRect().height;
      if (h > 0 && Math.abs(h - rowH) > 0.25) rowH = h;
    }
    const probe = el.querySelector<HTMLElement>(".char-probe");
    const chars = probe?.textContent?.length ?? 0;
    if (probe !== null && chars > 0) {
      const w = probe.getBoundingClientRect().width / chars;
      if (w > 0 && Math.abs(w - charW) > 0.05) {
        charW = w;
        // The measured font (or a new text size) re-derives every column the
        // user has not sized.
        resizeAutoColumns();
      }
    }
  }

  function resizeAutoColumns(): void {
    const auto = autoColumnWidths(columns, rows.slice(0, 200), charW || FALLBACK_CHAR_W, {
      pad: CELL_PAD_PX,
      min: MIN_COL_PX,
      maxChars: AUTO_MAX_CHARS,
    });
    widths = auto.map((w, c) => (userSized.has(c) && widths[c] !== undefined ? widths[c] : w));
  }

  /** Widen (never narrow — no jitter while scrolling) the columns the user
   *  has not sized so a newly loaded page's cells fit: ids deep in a file
   *  are longer than the first page's. Also adds columns for wider rows. */
  function growAutoColumns(sample: string[][]): void {
    const fit = autoColumnWidths(columns, sample.slice(0, 200), charW || FALLBACK_CHAR_W, {
      pad: CELL_PAD_PX,
      min: MIN_COL_PX,
      maxChars: AUTO_MAX_CHARS,
    });
    if (fit.length <= widths.length && fit.every((w, c) => userSized.has(c) || w <= widths[c])) return;
    widths = fit.map((w, c) => {
      const held = widths[c];
      if (held === undefined) return w;
      return userSized.has(c) ? held : Math.max(held, w);
    });
  }

  const NUMERIC_RE = /^-?(?:\d[\d,]*)(?:\.\d+)?(?:[eE][-+]?\d+)?%?$/;

  function detectNumeric(cols: string[], sample: string[][]): Set<number> {
    const out = new Set<number>();
    const width = Math.max(cols.length, ...sample.map((r) => r.length));
    for (let c = 0; c < width; c++) {
      let seen = 0;
      let numeric = 0;
      for (const row of sample) {
        const v = row[c];
        if (v === undefined || v === "") continue;
        seen++;
        if (NUMERIC_RE.test(v.trim())) numeric++;
      }
      if (seen > 0 && numeric / seen >= 0.8) out.add(c);
    }
    return out;
  }

  /** One page from the source, riding out budget-limited daemon scans (each
   *  must get further than the last). */
  async function fetchAt(
    offset: number,
    limit: number,
    progress?: (reached: number) => void,
  ): Promise<TablePage> {
    let reached = -1;
    let page: TablePage | null = null;
    for (let round = 0; round < MAX_SCAN_ROUNDS; round++) {
      page = fetchPage !== undefined ? await fetchPage(offset, limit) : await fsTable(path, offset, limit);
      if (page.scan_limited !== true) return page;
      const next = page.scanned_to ?? 0;
      if (next <= reached) break;
      reached = next;
      progress?.(next);
    }
    if (page === null) throw new Error("failed to load rows");
    throw new Error(`gave up scanning at row ${(page.scanned_to ?? 0).toLocaleString("en-US")}`);
  }

  function noteCounts(page: TablePage): void {
    if (typeof page.total_rows === "number") total = page.total_rows;
    if (typeof page.est_rows === "number") estimate = page.est_rows;
  }

  /**
   * Fold a page into the window. "replace" starts a new window at the page
   * (first load, refresh, jump); "append"/"prepend" extend the current one
   * and trim the far end past MAX_LOADED_ROWS, keeping the view still.
   */
  function apply(page: TablePage, how: "replace" | "append" | "prepend"): void {
    noteCounts(page);
    if (how === "replace") {
      const fresh = columns.length !== page.columns.length || columns.some((c, i) => c !== page.columns[i]);
      columns = page.columns;
      rows = page.rows;
      loadedOffset = page.offset;
      if (fresh || widths.length === 0) {
        numericCols = detectNumeric(page.columns, page.rows.slice(0, 50));
        userSized.clear();
        widths = [];
        resizeAutoColumns();
      }
      atEnd = !page.truncated;
      error = null;
      sel = null;
      expanded = null;
    } else if (how === "append") {
      let next = rows.concat(page.rows);
      atEnd = !page.truncated;
      if (next.length > MAX_LOADED_ROWS) {
        const drop = next.length - MAX_LOADED_ROWS;
        next = next.slice(drop);
        loadedOffset += drop;
        shiftScroll(-drop);
      }
      rows = next;
    } else {
      let next = page.rows.concat(rows);
      loadedOffset = page.offset;
      shiftScroll(page.rows.length);
      if (next.length > MAX_LOADED_ROWS) {
        next = next.slice(0, MAX_LOADED_ROWS);
        atEnd = false;
      }
      rows = next;
    }
    if (atEnd) total = loadedOffset + rows.length;
    growAutoColumns(page.rows);
  }

  /** Keep the same rows on screen after `delta` rows were added (+) above
   *  or removed (−) from above the view. */
  function shiftScroll(delta: number): void {
    const next = Math.max(0, scrollTop + delta * rowH);
    scrollTop = next;
    void tick().then(() => {
      if (scroller !== null) scroller.scrollTop = next;
    });
  }

  async function loadMore(): Promise<void> {
    if (loading || atEnd || error !== null || seeking !== null) return;
    loading = true;
    const p = path;
    const gen = jumpGen;
    try {
      const page = await fetchAt(loadedOffset + rows.length, pageRows());
      if (p !== path || gen !== jumpGen) return;
      apply(page, "append");
    } catch (e) {
      if (p !== path) return;
      error = e instanceof Error ? e.message : "failed to load more rows";
    } finally {
      if (p === path) loading = false;
    }
    void tick().then(maybePrefetch);
  }

  async function loadPrev(): Promise<void> {
    if (loading || loadedOffset === 0 || error !== null || seeking !== null) return;
    loading = true;
    const p = path;
    const gen = jumpGen;
    const offset = Math.max(0, loadedOffset - pageRows());
    try {
      const page = await fetchAt(offset, loadedOffset - offset);
      if (p !== path || gen !== jumpGen) return;
      apply(page, "prepend");
    } catch (e) {
      if (p !== path) return;
      error = e instanceof Error ? e.message : "failed to load rows";
    } finally {
      if (p === path) loading = false;
    }
  }

  function maybePrefetch(): void {
    if (rows.length === 0) return;
    if (win.end >= rows.length - PREFETCH_ROWS && !atEnd) void loadMore();
    else if (win.start <= PREFETCH_ROWS && loadedOffset > 0) void loadPrev();
  }

  function onScroll(): void {
    const el = scroller;
    if (el === null) return;
    scrollTop = el.scrollTop;
    expanded = null;
    maybePrefetch();
  }

  // --- jump to row -------------------------------------------------------------

  /** Scroll row `r` into view and flash it (through `to`, for a block). */
  function scrollToRow(r: number, to = r): void {
    const el = scroller;
    if (el === null) return;
    const i = r - loadedOffset;
    // Two rows of context above the target.
    const top = Math.max(0, (i - 2) * rowH);
    el.scrollTop = top;
    scrollTop = el.scrollTop;
    flashRows = { from: r, to: Math.max(r, to) };
    if (flashTimer !== null) clearTimeout(flashTimer);
    flashTimer = setTimeout(() => {
      flashTimer = null;
      flashRows = null;
    }, FLASH_MS);
  }

  /** Show 1-based row `n` (flashing through row `to`): a scroll when it is
   *  loaded, else a new window fetched around it (the daemon seeks via its
   *  row index). Resolves once the row shows. */
  async function jumpTo(n: number, to = n): Promise<void> {
    let target = n - 1;
    if (total !== null && total > 0) target = Math.min(target, total - 1);
    if (target >= loadedOffset && target < loadedOffset + rows.length) {
      scrollToRow(target, to - 1);
      return;
    }
    const gen = ++jumpGen;
    const p = path;
    seeking = `seeking row ${(target + 1).toLocaleString("en-US")}…`;
    try {
      let page = await fetchAt(jumpOffset(target + 1), pageRows(), (reached) => {
        if (gen === jumpGen) seeking = `indexing… row ${reached.toLocaleString("en-US")}`;
      });
      if (gen !== jumpGen || p !== path) return;
      noteCounts(page);
      if (page.rows.length === 0 && total !== null && total > 0) {
        // Past the end: land on the last row instead.
        target = total - 1;
        page = await fetchAt(jumpOffset(total), pageRows());
        if (gen !== jumpGen || p !== path) return;
      }
      if (page.rows.length === 0) {
        error = `row ${(n).toLocaleString("en-US")} is past the end`;
        return;
      }
      apply(page, "replace");
      target = Math.min(target, page.offset + page.rows.length - 1);
      await tick();
      scrollToRow(target, Math.max(target, to - 1));
      void tick().then(maybePrefetch);
    } catch (e) {
      if (gen === jumpGen && p === path) error = e instanceof Error ? e.message : "jump failed";
    } finally {
      if (gen === jumpGen) seeking = null;
    }
  }

  function onRowKey(e: KeyboardEvent): void {
    const input = e.currentTarget as HTMLInputElement;
    if (e.key === "Enter") {
      e.preventDefault();
      const n = parseRowNumber(input.value);
      rowDraft = null;
      if (n !== null) {
        error = null;
        void jumpTo(n);
      }
      input.select();
    } else if (e.key === "Escape") {
      rowDraft = null;
      input.blur();
    }
  }

  /** Honest about what is loaded: "of 5,000" when it all is, else
   *  "· 3,000 loaded rows of ~1.2M". */
  const countLabel = $derived.by(() => {
    if (rows.length === 0) return "· no rows";
    if (total !== null && atEnd && loadedOffset === 0) return `of ${formatRowCount(total, true)}`;
    const loaded = `· ${rows.length.toLocaleString("en-US")} loaded ${rows.length === 1 ? "row" : "rows"}`;
    if (total !== null) return `${loaded} of ${formatRowCount(total, true)}`;
    if (estimate !== null) return `${loaded} of ${formatRowCount(estimate, false)}`;
    return `${loaded} of ${(loadedOffset + rows.length).toLocaleString("en-US")}+`;
  });

  // --- column resize / auto-fit ---------------------------------------------
  let resizeCol: number | null = null;
  let resizeStartX = 0;
  let resizeStartW = 0;

  function onResizeDown(e: PointerEvent, col: number): void {
    e.preventDefault();
    e.stopPropagation();
    resizeCol = col;
    resizeStartX = e.clientX;
    resizeStartW = widths[col] ?? MIN_COL_PX;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }

  function onResizeMove(e: PointerEvent): void {
    if (resizeCol === null) return;
    const next = Math.max(MIN_COL_PX, resizeStartW + (e.clientX - resizeStartX));
    const col = resizeCol;
    userSized.add(col);
    widths = widths.map((w, c) => (c === col ? next : w));
  }

  function onResizeUp(e: PointerEvent): void {
    if (resizeCol === null) return;
    resizeCol = null;
    (e.currentTarget as HTMLElement).releasePointerCapture?.(e.pointerId);
  }

  /** Double-click a divider: fit the column to its widest loaded cell. */
  function autoFit(col: number): void {
    const sample = rows.slice(0, 5_000).map((r) => [r[col] ?? ""]);
    const [fit] = autoColumnWidths([columns[col] ?? ""], sample, charW || FALLBACK_CHAR_W, {
      pad: CELL_PAD_PX,
      min: MIN_COL_PX,
      maxChars: AUTOFIT_MAX_CHARS,
    });
    userSized.add(col);
    widths = widths.map((w, c) => (c === col ? fit : w));
  }

  // --- cell / row selection + copy ------------------------------------------
  function norm(s: Sel): Sel {
    return {
      r0: Math.min(s.r0, s.r1),
      r1: Math.max(s.r0, s.r1),
      c0: Math.min(s.c0, s.c1),
      c1: Math.max(s.c0, s.c1),
    };
  }

  const selN = $derived(sel === null ? null : norm(sel));

  function inSel(r: number, c: number): boolean {
    const s = selN;
    return s !== null && r >= s.r0 && r <= s.r1 && c >= s.c0 && c <= s.c1;
  }

  function onCellDown(e: PointerEvent, r: number, c: number): void {
    if (e.button !== 0) return;
    selecting = true;
    hit = null;
    if (e.shiftKey && anchor !== null) {
      sel = { r0: anchor.r, c0: anchor.c, r1: r, c1: c };
    } else {
      anchor = { r, c };
      sel = { r0: r, c0: c, r1: r, c1: c };
    }
  }

  function onCellEnter(r: number, c: number): void {
    if (!selecting || anchor === null) return;
    sel = { r0: anchor.r, c0: anchor.c, r1: r, c1: c };
    // Leaving the first cell makes it a block drag: the text selection a
    // press inside one cell starts (for copying part of it) must not smear
    // across the grid under the block highlight.
    if (!blockDrag && (r !== anchor.r || c !== anchor.c)) {
      blockDrag = true;
      window.getSelection()?.removeAllRanges();
    }
  }

  function onRowGutterDown(e: PointerEvent, r: number): void {
    if (e.button !== 0) return;
    selecting = true;
    hit = null;
    const lastC = widths.length - 1;
    if (e.shiftKey && anchor !== null) {
      sel = { r0: anchor.r, c0: 0, r1: r, c1: lastC };
    } else {
      anchor = { r, c: 0 };
      sel = { r0: r, c0: 0, r1: r, c1: lastC };
    }
  }

  function onGutterEnter(r: number): void {
    if (!selecting || anchor === null) return;
    sel = { r0: anchor.r, c0: 0, r1: r, c1: widths.length - 1 };
  }

  function endSelect(): void {
    selecting = false;
    blockDrag = false;
  }

  /** Global pointer-up: end any in-flight cell drag or column resize. */
  function onWindowPointerUp(): void {
    selecting = false;
    blockDrag = false;
    resizeCol = null;
  }

  /** The selected block as TSV — only the part still in the loaded window. */
  function selectionText(): string {
    if (selN === null) return "";
    const s = selN;
    const lines: string[] = [];
    const from = Math.max(s.r0, loadedOffset);
    const to = Math.min(s.r1, loadedOffset + rows.length - 1);
    for (let r = from; r <= to; r++) {
      const row = rows[r - loadedOffset];
      const cells: string[] = [];
      for (let c = s.c0; c <= s.c1; c++) cells.push(row?.[c] ?? "");
      lines.push(cells.join("\t"));
    }
    return lines.join("\n");
  }

  function onKeyDown(e: KeyboardEvent): void {
    if ((e.metaKey || e.ctrlKey) && (e.key === "c" || e.key === "C")) {
      if (window.getSelection()?.toString()) return; // native text selection wins
      const text = selectionText();
      if (text !== "") {
        void copyText(text);
        e.preventDefault();
      }
    } else if ((e.metaKey || e.ctrlKey) && (e.key === "a" || e.key === "A")) {
      if (rows.length > 0) {
        anchor = { r: loadedOffset, c: 0 };
        sel = { r0: loadedOffset, c0: 0, r1: loadedOffset + rows.length - 1, c1: widths.length - 1 };
        e.preventDefault();
      }
    } else if (e.key === "Escape") {
      if (expanded !== null) expanded = null;
      else if (sel !== null) sel = null;
      else hit = null;
    }
  }

  // --- expand a cell -----------------------------------------------------------

  function openCell(e: MouseEvent, r: number, c: number): void {
    const cell = e.currentTarget as HTMLElement;
    const host = root;
    if (host === null) return;
    window.getSelection()?.removeAllRanges();
    const box = cell.getBoundingClientRect();
    const frame = host.getBoundingClientRect();
    const popW = Math.min(520, frame.width - 16);
    const left = Math.min(Math.max(8, box.left - frame.left), Math.max(8, frame.width - popW - 8));
    // Below the cell, or above it when the cell sits in the lower half.
    const below = box.bottom - frame.top + 4;
    const top = below < frame.height / 2 ? below : Math.max(8, box.top - frame.top - 4);
    copied = false;
    expanded = {
      r,
      c,
      column: columns[c] ?? `col${c + 1}`,
      text: rows[r - loadedOffset]?.[c] ?? "",
      left,
      top,
    };
  }

  async function copyExpanded(): Promise<void> {
    if (expanded === null) return;
    await copyText(expanded.text);
    copied = true;
  }

  /** Close the popover on any press outside it. */
  function onWindowPointerDown(e: PointerEvent): void {
    if (expanded === null) return;
    const target = e.target as Node | null;
    const pop = root?.querySelector(".cell-pop");
    if (pop !== null && pop !== undefined && target !== null && pop.contains(target)) return;
    expanded = null;
  }

  function onWindowKey(e: KeyboardEvent): void {
    if (expanded !== null && e.key === "Escape") {
      expanded = null;
      scroller?.focus();
    }
  }

  // --- pointing at a block (context bridge) ---------------------------------------
  //
  // A settled selection goes to an agent as its locator (`#cell=5,2-9,4`,
  // `#row=5-9`, or the host's sheet + A1 form) plus its values as a small
  // one-line TSV (header first, capped at 50 rows × 20 columns / 8 KB).

  const selOwner = {};
  let published: FileSelection | null = null;
  /** Cmd+C's text rides along (copy provenance) only for modest blocks. */
  const PROVENANCE_MAX_CELLS = 5_000;
  const CHIP_W = 170;

  const defaultLocate = (b: TableBlock) => ({ fragment: tableFragment(b), label: tableLabel(b) });

  /** The block, 1-based, as the locator helpers take it. */
  function blockOf(s: Sel): TableBlock {
    return {
      r0: s.r0 + 1,
      r1: s.r1 + 1,
      c0: s.c0 + 1,
      c1: s.c1 + 1,
      wholeRows: s.c0 === 0 && s.c1 === widths.length - 1,
    };
  }

  /** The block's values: the loaded part of it, honest about the rest. */
  function blockQuote(s: Sel): string {
    const from = Math.max(s.r0, loadedOffset);
    const to = Math.min(s.r1, loadedOffset + rows.length - 1, from + TSV_LIMITS.maxRows);
    const out: string[][] = [];
    for (let r = from; r <= to; r++) out.push((rows[r - loadedOffset] ?? []).slice(s.c0, s.c1 + 1));
    const header = columns.length > 0 ? columns.slice(s.c0, s.c1 + 1) : null;
    const clippedAfter = s.r1 > loadedOffset + rows.length - 1 && out.length <= TSV_LIMITS.maxRows;
    return tsvQuote(header, out, TSV_LIMITS, s.r0 < loadedOffset, clippedAfter);
  }

  function publishBlock(s: Sel | null): void {
    if (s === null || rows.length === 0 || widths.length === 0) {
      if (published !== null) {
        published = null;
        clearSelection(selOwner);
      }
      return;
    }
    const b = blockOf(s);
    const { fragment, label } = (locate ?? defaultLocate)(b);
    const cells = (s.r1 - s.r0 + 1) * (s.c1 - s.c0 + 1);
    const sel: FileSelection = {
      kind: "file",
      path,
      startLine: null,
      endLine: null,
      text: cells <= PROVENANCE_MAX_CELLS ? selectionText() : "",
      fragment,
      quote: blockQuote(s),
      label,
    };
    published = sel;
    setSelection(selOwner, sel);
  }

  // Publish once a drag settles (and on every keyboard/gutter change).
  $effect(() => {
    const s = selN;
    if (selecting) return;
    untrack(() => publishBlock(s));
  });

  // A newer selection elsewhere takes over: this block's chip goes (the
  // block itself stays selected for copying).
  let superseded = $state(false);
  $effect(() => {
    const a = $activeSelection;
    superseded = published !== null && a !== published;
  });

  $effect(() => () => {
    if (published !== null) clearSelection(selOwner);
    published = null;
  });

  const chipLabel = $derived(selN === null || widths.length === 0 ? undefined : (locate ?? defaultLocate)(blockOf(selN)).label);

  /** The chip, under the block's last row, right-aligned to its last column
   *  (content coordinates: it scrolls with the grid). */
  const chipPos = $derived.by(() => {
    const s = selN;
    if (s === null || selecting || superseded || rows.length === 0) return null;
    const last = Math.min(Math.max(s.r1 - loadedOffset, 0), rows.length - 1);
    const headH = head?.offsetHeight ?? rowH;
    let left = gutterW;
    for (let c = 0; c < s.c0; c++) left += widths[c] ?? 0;
    let right = left;
    for (let c = s.c0; c <= s.c1; c++) right += widths[c] ?? 0;
    return { x: Math.max(left + 4, right - CHIP_W), y: headH + (last + 1) * rowH + 4 };
  });

  // --- landing on a block (`#row=`, `#col=`, `#cell=`) -----------------------------

  let revealGen = 0;

  /** Jump to a revealed block, flash its rows and outline its cells. */
  async function revealBlock(t: NonNullable<Reveal["table"]>): Promise<void> {
    const gen = ++revealGen;
    const lastC = Math.max(0, widths.length - 1);
    const c0 = Math.min((t.col ?? 1) - 1, lastC);
    const c1 = t.col === undefined ? lastC : Math.min((t.endCol ?? t.col) - 1, lastC);
    const firstRow = t.row ?? 1;
    const lastRow = t.row === undefined ? firstRow : (t.endRow ?? t.row);
    await jumpTo(firstRow, lastRow);
    if (gen !== revealGen) return;
    const r0 = firstRow - 1;
    const r1 = t.row === undefined ? loadedOffset + rows.length - 1 : lastRow - 1;
    hit = { r0, r1, c0, c1 };
    // Bring the first column into view when it is off to the side.
    const el = scroller;
    if (el !== null && t.col !== undefined) {
      let x = gutterW;
      for (let c = 0; c < c0; c++) x += widths[c] ?? 0;
      if (x < el.scrollLeft + gutterW || x > el.scrollLeft + el.clientWidth - 60) {
        el.scrollLeft = Math.max(0, x - gutterW - 24);
      }
    }
  }

  function inHit(r: number, c: number): boolean {
    const h = hit;
    return h !== null && r >= h.r0 && r <= h.r1 && c >= h.c0 && c <= h.c1;
  }

  // CSV/TSV: take `#row=`/`#col=`/`#cell=` reveals once the first page is
  // in (the xlsx host hands its own down instead).
  $effect(() => {
    void $revealRequest;
    if (fetchPage !== undefined || rows.length === 0 || widths.length === 0) return;
    const req = takeReveal(path);
    const t = req?.table;
    if (t === undefined) return;
    untrack(() => void revealBlock(t));
  });

  let seenReveal = 0;
  $effect(() => {
    const r = reveal;
    if (r === null || r.nonce === seenReveal || rows.length === 0 || widths.length === 0) return;
    seenReveal = r.nonce;
    untrack(() => void revealBlock(r.table));
  });
</script>

<svelte:window onpointerup={onWindowPointerUp} onpointerdown={onWindowPointerDown} onkeydown={onWindowKey} />

<div class="table-view" bind:this={root}>
  {#if error !== null && rows.length === 0}
    <div class="file-error">{error}</div>
  {:else if loading && rows.length === 0}
    <Spinner />
  {:else}
    <div
      class="scroll"
      bind:this={scroller}
      onscroll={onScroll}
      onpointerup={endSelect}
      onpointerleave={endSelect}
      onkeydown={onKeyDown}
      tabindex="0"
      role="grid"
      aria-rowcount={total ?? -1}
    >
      <span class="char-probe" aria-hidden="true">0000000000</span>
      {#if chipPos !== null}
        <ReferenceChip x={chipPos.x} y={chipPos.y} label={chipLabel} />
      {/if}
      <!-- The last, unsized column takes whatever the pane has left, so the
           sized ones keep their widths instead of being stretched. -->
      <table class:block-drag={blockDrag} style:width={`max(100%, ${tableW}px)`}>
        <colgroup>
          <col style:width={`${gutterW}px`} />
          {#each widths as w, c (c)}
            <col style:width={`${w}px`} />
          {/each}
          <col />
        </colgroup>
        <thead bind:this={head}>
          <tr>
            <th class="ln gut" aria-label="row number"></th>
            {#each widths as _, c (c)}
              <th class:num={numericCols.has(c)} title={columns[c] ?? ""}>
                <span class="th-label">{columns[c] ?? ""}</span>
                <span
                  class="resizer"
                  role="separator"
                  aria-label="resize column"
                  aria-orientation="vertical"
                  onpointerdown={(e) => onResizeDown(e, c)}
                  onpointermove={onResizeMove}
                  onpointerup={onResizeUp}
                  ondblclick={() => autoFit(c)}
                ></span>
              </th>
            {/each}
            <th class="fill" aria-hidden="true"></th>
          </tr>
        </thead>
        <tbody>
          {#if win.padTop > 0}
            <tr class="spacer" aria-hidden="true"><td colspan={widths.length + 2} style:height={`${win.padTop}px`}></td></tr>
          {/if}
          {#each visibleRows as row, i (loadedOffset + win.start + i)}
            {@const r = loadedOffset + win.start + i}
            <tr class="row" class:flash={flashRows !== null && r >= flashRows.from && r <= flashRows.to}>
              <td
                class="ln gut"
                class:selrow={inSel(r, 0)}
                onpointerdown={(e) => onRowGutterDown(e, r)}
                onpointerenter={() => onGutterEnter(r)}>{(r + 1).toLocaleString("en-US")}</td
              >
              {#each widths as _, c (c)}
                <td
                  class:num={numericCols.has(c)}
                  class:sel={inSel(r, c)}
                  class:hit={inHit(r, c)}
                  onpointerdown={(e) => onCellDown(e, r, c)}
                  onpointerenter={() => onCellEnter(r, c)}
                  ondblclick={(e) => openCell(e, r, c)}>{row[c] ?? ""}</td
                >
              {/each}
              <td class="fill"></td>
            </tr>
          {/each}
          {#if win.padBottom > 0}
            <tr class="spacer" aria-hidden="true"
              ><td colspan={widths.length + 2} style:height={`${win.padBottom}px`}></td></tr
            >
          {/if}
        </tbody>
      </table>
      {#if loading && rows.length > 0 && !atEnd}
        <div class="more">loading more…</div>
      {/if}
    </div>
    <footer class="pager">
      <span class="pos">
        <span class="pos-label">row</span>
        <input
          class="row-input"
          type="text"
          inputmode="numeric"
          aria-label="row (enter a number to jump)"
          title="row — type a number and press Enter to jump"
          size={Math.max(String(total ?? estimate ?? loadedOffset + rows.length).length + 2, 4)}
          value={rowDraft ?? topRow.toLocaleString("en-US")}
          onfocus={(e) => (e.currentTarget as HTMLInputElement).select()}
          oninput={(e) => (rowDraft = (e.currentTarget as HTMLInputElement).value)}
          onkeydown={onRowKey}
          onblur={() => (rowDraft = null)}
        />
        <span class="count">{countLabel}</span>
      </span>
      <span class="spacer"></span>
      {#if seeking !== null}
        <span class="seeking">{seeking}</span>
      {:else if error !== null}
        <span class="err">{error}</span>
      {:else if sel !== null}
        <span class="selnote">⌘/Ctrl+C copies the selection · double-click a cell to expand</span>
      {/if}
    </footer>
    {#if expanded !== null}
      <div
        class="cell-pop"
        role="dialog"
        aria-label={`${expanded.column}, row ${(expanded.r + 1).toLocaleString("en-US")}`}
        style:left={`${expanded.left}px`}
        style:top={`${expanded.top}px`}
      >
        <div class="cp-head">
          <span class="cp-col" title={expanded.column}>{expanded.column}</span>
          <span class="cp-row">row {(expanded.r + 1).toLocaleString("en-US")} · {expanded.text.length.toLocaleString("en-US")} chars</span>
          <span class="spacer"></span>
          <button class="cp-btn" onclick={copyExpanded}>{copied ? "copied" : "copy"}</button>
          <button class="cp-btn ic" aria-label="close" title="close (Esc)" onclick={() => (expanded = null)}>×</button>
        </div>
        <pre class="cp-text">{expanded.text === "" ? "(empty)" : expanded.text}</pre>
      </div>
    {/if}
  {/if}
</div>

<style>
  .table-view {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
  }

  .scroll {
    position: relative;
    flex: 1;
    overflow: auto;
    min-height: 0;
    outline: none;
    overflow-anchor: none;
    scrollbar-width: thin;
    scrollbar-color: color-mix(in srgb, var(--fg) 22%, transparent) transparent;
  }

  .char-probe {
    position: absolute;
    visibility: hidden;
    white-space: pre;
    font-family: var(--mono);
    font-size: var(--text-sm);
    pointer-events: none;
  }

  table {
    table-layout: fixed;
    border-collapse: separate;
    border-spacing: 0;
    font-family: var(--mono);
    font-size: var(--text-sm);
    line-height: 1.4;
  }

  td.fill,
  th.fill {
    padding: 0;
    cursor: default;
  }

  thead th {
    position: sticky;
    top: 0;
    z-index: 2;
    background: var(--term-bg);
    text-align: left;
    font-weight: 600;
    color: var(--fg);
    padding: 0.45rem 0.9rem 0.4rem 0.6rem;
    border-bottom: 1px solid var(--edge);
    white-space: nowrap;
    overflow: hidden;
    box-shadow: 0 1px 0 var(--edge);
  }

  thead th.num .th-label {
    display: block;
    text-align: right;
  }

  /* line-number column is sticky both top (header) and left (gutter). */
  .gut {
    position: sticky;
    left: 0;
    z-index: 1;
  }

  thead th.gut {
    z-index: 3;
  }

  td.gut {
    background: var(--term-bg);
    cursor: pointer;
  }

  .th-label {
    display: block;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .resizer {
    position: absolute;
    top: 0;
    right: 0;
    width: 9px;
    height: 100%;
    cursor: col-resize;
    touch-action: none;
    user-select: none;
  }

  .resizer::after {
    content: "";
    position: absolute;
    top: 20%;
    right: 4px;
    width: 1px;
    height: 60%;
    background: transparent;
    transition: background-color 0.12s ease;
  }

  th:hover .resizer::after {
    background: color-mix(in srgb, var(--fg) 28%, transparent);
  }

  td {
    padding: 0.22rem 0.9rem 0.22rem 0.6rem;
    color: var(--fg);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    border-bottom: 1px solid color-mix(in srgb, var(--edge) 45%, transparent);
    cursor: cell;
  }

  tr.spacer td {
    padding: 0;
    border: none;
  }

  td.num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }

  table.block-drag {
    user-select: none;
  }

  td.sel,
  td.selrow {
    background: color-mix(in srgb, var(--accent, #4a90d9) 22%, transparent);
    color: var(--fg);
  }

  /* A revealed block (a `#cell=` link): outlined until the next press. */
  td.hit:not(.sel) {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--accent) 55%, transparent);
  }

  .ln {
    color: var(--muted);
    text-align: right;
    padding-left: 0.6rem;
    user-select: none;
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }

  td.ln {
    color: color-mix(in srgb, var(--muted) 70%, transparent);
  }

  .ln.selrow {
    color: var(--fg);
  }

  tr.row:hover td:not(.sel):not(.selrow) {
    background: color-mix(in srgb, var(--fg) 3.5%, transparent);
  }

  tr.row:hover td.gut {
    background: color-mix(in srgb, var(--fg) 6%, var(--term-bg));
  }

  tr.flash td {
    animation: row-flash 1.4s ease-out;
  }

  @keyframes row-flash {
    0%,
    35% {
      background: color-mix(in srgb, var(--accent) 24%, var(--term-bg));
    }
    100% {
      background: transparent;
    }
  }

  .more {
    position: sticky;
    left: 0;
    padding: 0.5rem;
    text-align: center;
    color: var(--muted);
    font-size: var(--text-xs);
    font-family: var(--mono);
  }

  .pager {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.6rem;
    height: 28px;
    padding: 0 0.7rem;
    border-top: 1px solid var(--edge);
    font-size: var(--text-xs);
    color: var(--muted);
    white-space: nowrap;
    overflow: hidden;
  }

  .pos {
    display: inline-flex;
    align-items: center;
    gap: 0.35em;
    font-variant-numeric: tabular-nums;
    min-width: 0;
  }

  .row-input {
    appearance: none;
    box-sizing: content-box;
    padding: 0 0.3em;
    border: 1px solid transparent;
    border-radius: 4px;
    background: none;
    font: inherit;
    font-variant-numeric: tabular-nums;
    color: var(--fg);
    text-align: right;
  }

  .row-input:hover {
    border-color: var(--edge);
  }

  .row-input:focus {
    outline: none;
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
    background: var(--term-bg);
  }

  .count {
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .selnote,
  .seeking {
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .seeking {
    color: var(--fg);
  }

  .err {
    color: var(--danger, #d9534f);
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .spacer {
    flex: 1;
  }

  .cell-pop {
    position: absolute;
    z-index: 10;
    width: min(520px, calc(100% - 16px));
    max-height: 50%;
    display: flex;
    flex-direction: column;
    background: var(--term-bg);
    border: 1px solid var(--edge);
    border-radius: 6px;
    box-shadow: 0 8px 28px rgba(0, 0, 0, 0.22);
    overflow: hidden;
  }

  .cp-head {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.3rem 0.4rem 0.3rem 0.65rem;
    border-bottom: 1px solid var(--edge);
    font-size: var(--text-xs);
    color: var(--muted);
    white-space: nowrap;
  }

  .cp-col {
    font-family: var(--mono);
    font-weight: 600;
    color: var(--fg);
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
  }

  .cp-row {
    font-variant-numeric: tabular-nums;
  }

  .cp-btn {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    color: var(--muted);
    cursor: pointer;
    padding: 0.1rem 0.45rem;
    border-radius: 4px;
  }

  .cp-btn:hover {
    background: var(--row-hover);
    color: var(--fg);
  }

  .cp-btn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }

  .cp-btn.ic {
    font-size: var(--text-lg);
    line-height: 1;
  }

  .cp-text {
    margin: 0;
    padding: 0.55rem 0.7rem;
    overflow: auto;
    font-family: var(--mono);
    font-size: var(--text-sm);
    line-height: 1.45;
    color: var(--fg);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    user-select: text;
  }

  .file-error {
    margin: auto;
    color: var(--muted);
    font-size: var(--text-md);
    padding: 1rem;
    text-align: center;
  }

  @media (prefers-reduced-motion: reduce) {
    tr.flash td {
      animation: none;
      background: color-mix(in srgb, var(--accent) 18%, var(--term-bg));
    }
  }
</style>
