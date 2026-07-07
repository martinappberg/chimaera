<script lang="ts">
  import { fsTable, type TablePage } from "./files";
  import { getSetting } from "./settings/store.svelte";

  interface Props {
    path: string;
  }

  let { path }: Props = $props();

  /** Rows per fetched page (settings ground truth, read per request). */
  const pageRows = () => getSetting("files.tableRowsPerPage");
  /** Start fetching the next page when the scroll gets this close to the end. */
  const PREFETCH_PX = 600;
  const MIN_COL_PX = 48;
  const MAX_AUTOFIT_PX = 480;

  let columns = $state<string[]>([]);
  let rows = $state<string[][]>([]);
  let total = $state<number | null>(null); // null = unknown / more beyond
  let loadedOffset = $state(0); // rows before our first loaded row (always 0 here)
  let error = $state<string | null>(null);
  let loading = $state(false);
  let atEnd = $state(false);
  let scroller = $state<HTMLDivElement | null>(null);

  /** Explicit per-column widths (px); unset = auto (content-driven). */
  let widths = $state<Record<number, number>>({});
  /** Columns detected as numeric get right-aligned tabular figures. */
  let numericCols = $state<Set<number>>(new Set());

  /** Selection: a rectangular block of cells, or whole rows via the gutter. */
  interface Sel {
    r0: number;
    c0: number;
    r1: number;
    c1: number;
  }
  let sel = $state<Sel | null>(null);
  let anchor: { r: number; c: number } | null = null;
  let selecting = false;

  $effect(() => {
    const p = path;
    // reset everything for the new file
    columns = [];
    rows = [];
    total = null;
    loadedOffset = 0;
    error = null;
    atEnd = false;
    widths = {};
    numericCols = new Set();
    sel = null;
    anchor = null;
    void loadFirst(p);
  });

  const NUMERIC_RE = /^-?(?:\d[\d,]*)(?:\.\d+)?(?:[eE][-+]?\d+)?%?$/;

  function detectNumeric(cols: string[], sample: string[][]): Set<number> {
    const out = new Set<number>();
    for (let c = 0; c < cols.length; c++) {
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

  async function loadFirst(p: string): Promise<void> {
    loading = true;
    try {
      const page = await fsTable(p, 0, pageRows());
      if (p !== path) return;
      apply(page, true);
      scroller?.scrollTo({ top: 0 });
    } catch (e) {
      if (p !== path) return;
      error = e instanceof Error ? e.message : "failed to load table";
    } finally {
      if (p === path) loading = false;
    }
  }

  /** Append the next page without disturbing the current scroll position. */
  async function loadMore(): Promise<void> {
    if (loading || atEnd || error !== null) return;
    loading = true;
    const p = path;
    const off = loadedOffset + rows.length;
    try {
      const page = await fsTable(p, off, pageRows());
      if (p !== path) return;
      apply(page, false);
    } catch (e) {
      if (p !== path) return;
      error = e instanceof Error ? e.message : "failed to load more rows";
    } finally {
      if (p === path) loading = false;
    }
  }

  function apply(page: TablePage, first: boolean): void {
    if (first) {
      columns = page.columns;
      rows = page.rows;
      loadedOffset = page.offset;
      numericCols = detectNumeric(page.columns, page.rows.slice(0, 50));
    } else {
      rows = rows.concat(page.rows);
    }
    if (!page.truncated) {
      atEnd = true;
      total = loadedOffset + rows.length;
    }
  }

  function onScroll(): void {
    const el = scroller;
    if (el === null || atEnd || loading) return;
    if (el.scrollTop + el.clientHeight >= el.scrollHeight - PREFETCH_PX) {
      void loadMore();
    }
  }

  // The row currently at the top of the viewport — "position always visible".
  let topRow = $state(1);
  function updateTopRow(): void {
    const el = scroller;
    if (el === null || rows.length === 0) return;
    const body = el.querySelector<HTMLElement>("tbody");
    if (body === null) return;
    const rowH = body.offsetHeight / rows.length;
    if (rowH <= 0) return;
    // Header is sticky, so scrollTop 0 sits at the first body row.
    const idx = Math.floor(el.scrollTop / rowH);
    topRow = Math.min(rows.length, Math.max(1, idx + 1));
  }

  function onScrollAll(): void {
    onScroll();
    updateTopRow();
  }

  const positionLabel = $derived.by(() => {
    if (rows.length === 0) return "no rows";
    const shown = rows.length.toLocaleString();
    const totalStr = total !== null ? total.toLocaleString() : `${shown}+`;
    const first = (loadedOffset + topRow).toLocaleString();
    return `row ${first} · ${shown} of ${totalStr} loaded`;
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
    const th = (e.currentTarget as HTMLElement).closest("th");
    resizeStartW = th?.getBoundingClientRect().width ?? MIN_COL_PX;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }

  function onResizeMove(e: PointerEvent): void {
    if (resizeCol === null) return;
    const next = Math.max(MIN_COL_PX, resizeStartW + (e.clientX - resizeStartX));
    widths = { ...widths, [resizeCol]: next };
  }

  function onResizeUp(e: PointerEvent): void {
    if (resizeCol === null) return;
    resizeCol = null;
    (e.currentTarget as HTMLElement).releasePointerCapture?.(e.pointerId);
  }

  /** Double-click a divider: auto-fit the column to its widest visible cell. */
  function autoFit(col: number): void {
    const el = scroller;
    if (el === null) return;
    const measure = document.createElement("span");
    measure.style.cssText =
      "position:absolute;visibility:hidden;white-space:pre;font-family:var(--mono);font-size:0.74rem;";
    el.appendChild(measure);
    let max = 0;
    measure.textContent = columns[col] ?? "";
    max = measure.offsetWidth;
    // sample up to 300 rows to keep it snappy on big tables
    const n = Math.min(rows.length, 300);
    for (let r = 0; r < n; r++) {
      measure.textContent = rows[r][col] ?? "";
      if (measure.offsetWidth > max) max = measure.offsetWidth;
    }
    measure.remove();
    const w = Math.min(MAX_AUTOFIT_PX, Math.max(MIN_COL_PX, max + 28));
    widths = { ...widths, [col]: w };
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

  function inSel(r: number, c: number): boolean {
    if (sel === null) return false;
    const s = norm(sel);
    return r >= s.r0 && r <= s.r1 && c >= s.c0 && c <= s.c1;
  }

  function onCellDown(e: PointerEvent, r: number, c: number): void {
    if (e.button !== 0) return;
    selecting = true;
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
  }

  function onRowGutterDown(e: PointerEvent, r: number): void {
    if (e.button !== 0) return;
    selecting = true;
    const lastC = columns.length - 1;
    if (e.shiftKey && anchor !== null) {
      sel = { r0: anchor.r, c0: 0, r1: r, c1: lastC };
    } else {
      anchor = { r, c: 0 };
      sel = { r0: r, c0: 0, r1: r, c1: lastC };
    }
  }

  function onGutterEnter(r: number): void {
    if (!selecting || anchor === null) return;
    sel = { r0: anchor.r, c0: 0, r1: r, c1: columns.length - 1 };
  }

  function endSelect(): void {
    selecting = false;
  }

  /** Global pointer-up: end any in-flight cell drag or column resize. */
  function onWindowPointerUp(): void {
    selecting = false;
    resizeCol = null;
  }

  function selectionText(): string {
    if (sel === null) return "";
    const s = norm(sel);
    const lines: string[] = [];
    for (let r = s.r0; r <= s.r1 && r < rows.length; r++) {
      const cells: string[] = [];
      for (let c = s.c0; c <= s.c1; c++) cells.push(rows[r]?.[c] ?? "");
      lines.push(cells.join("\t"));
    }
    return lines.join("\n");
  }

  function onKeyDown(e: KeyboardEvent): void {
    if ((e.metaKey || e.ctrlKey) && (e.key === "c" || e.key === "C")) {
      const text = selectionText();
      if (text !== "") {
        void navigator.clipboard?.writeText(text);
        e.preventDefault();
      }
    } else if ((e.metaKey || e.ctrlKey) && (e.key === "a" || e.key === "A")) {
      if (rows.length > 0) {
        anchor = { r: 0, c: 0 };
        sel = { r0: 0, c0: 0, r1: rows.length - 1, c1: columns.length - 1 };
        e.preventDefault();
      }
    } else if (e.key === "Escape") {
      sel = null;
    }
  }
</script>

<svelte:window onpointerup={onWindowPointerUp} />

<div class="table-view">
  {#if error !== null && rows.length === 0}
    <div class="file-error">{error}</div>
  {:else}
    <div
      class="scroll"
      bind:this={scroller}
      onscroll={onScrollAll}
      onpointerup={endSelect}
      onpointerleave={endSelect}
      onkeydown={onKeyDown}
      tabindex="0"
      role="grid"
    >
      <table style:table-layout={Object.keys(widths).length > 0 ? "fixed" : "auto"}>
        <thead>
          <tr>
            <th class="ln gut" aria-label="row number"></th>
            {#each columns as col, c (c)}
              <th
                class:num={numericCols.has(c)}
                style:width={widths[c] !== undefined ? `${widths[c]}px` : undefined}
                style:min-width={widths[c] !== undefined ? `${widths[c]}px` : undefined}
              >
                <span class="th-label">{col}</span>
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
          </tr>
        </thead>
        <tbody>
          {#each rows as row, r (r)}
            <tr>
              <td
                class="ln gut"
                class:selrow={inSel(r, 0)}
                onpointerdown={(e) => onRowGutterDown(e, r)}
                onpointerenter={() => onGutterEnter(r)}>{(loadedOffset + r + 1).toLocaleString()}</td
              >
              {#each row as cell, c (c)}
                <td
                  class:num={numericCols.has(c)}
                  class:sel={inSel(r, c)}
                  style:width={widths[c] !== undefined ? `${widths[c]}px` : undefined}
                  style:max-width={widths[c] !== undefined ? `${widths[c]}px` : undefined}
                  onpointerdown={(e) => onCellDown(e, r, c)}
                  onpointerenter={() => onCellEnter(r, c)}>{cell}</td
                >
              {/each}
            </tr>
          {/each}
        </tbody>
      </table>
      {#if loading && rows.length > 0}
        <div class="more">loading more…</div>
      {/if}
    </div>
    <footer class="pager">
      <span class="range">{positionLabel}</span>
      <span class="spacer"></span>
      {#if sel !== null}
        <span class="selnote">selection copied with ⌘/Ctrl+C</span>
      {/if}
      {#if error !== null}
        <span class="err">{error}</span>
      {/if}
    </footer>
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
    flex: 1;
    overflow: auto;
    min-height: 0;
    outline: none;
    scrollbar-width: thin;
    scrollbar-color: color-mix(in srgb, var(--fg) 22%, transparent) transparent;
  }

  table {
    border-collapse: separate;
    border-spacing: 0;
    font-family: var(--mono);
    font-size: 0.74rem;
    line-height: 1.4;
    min-width: 100%;
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
    display: inline-block;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 100%;
    vertical-align: bottom;
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
    max-width: 40ch;
    overflow: hidden;
    text-overflow: ellipsis;
    border-bottom: 1px solid color-mix(in srgb, var(--edge) 45%, transparent);
    cursor: cell;
  }

  td.num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }

  td.sel,
  td.selrow {
    background: color-mix(in srgb, var(--accent, #4a90d9) 22%, transparent);
    color: var(--fg);
  }

  .ln {
    color: var(--muted);
    opacity: 0.65;
    text-align: right;
    padding-left: 0.9rem;
    user-select: none;
    font-size: 0.66rem;
  }

  .ln.selrow {
    opacity: 1;
    color: var(--fg);
  }

  tbody tr:hover td:not(.sel):not(.selrow) {
    background: color-mix(in srgb, var(--fg) 3.5%, transparent);
  }

  tbody tr:hover td.gut {
    background: color-mix(in srgb, var(--fg) 6%, var(--term-bg));
  }

  .more {
    padding: 0.5rem;
    text-align: center;
    color: var(--muted);
    font-size: 0.68rem;
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
    font-size: 0.68rem;
    color: var(--muted);
  }

  .range {
    font-variant-numeric: tabular-nums;
  }

  .selnote {
    color: var(--muted);
    opacity: 0.85;
  }

  .err {
    color: var(--danger, #d9534f);
  }

  .spacer {
    flex: 1;
  }

  .file-error {
    margin: auto;
    color: var(--muted);
    font-size: 0.8rem;
    padding: 1rem;
    text-align: center;
  }
</style>
