<script lang="ts">
  /**
   * Spreadsheet (xlsx/xls/xlsm/ods) preview. The daemon parses the workbook
   * server-side (calamine) into the same paged `TablePage` the CSV viewer
   * renders, so this is a thin shell: a sheet picker on top, the shared
   * `TableView` grid below (selection / resize / infinite-scroll paging all come
   * from it). Each sheet's first page — fetched here anyway to learn the sheet
   * list — seeds the grid, and `fetchPage` pulls further pages of the same sheet.
   * Bounded by the server's source-size cap; no in-place editing (a spreadsheet
   * is not a text file).
   */
  import { fsXlsx, type TablePage } from "./files";
  import TableView from "./TableView.svelte";
  import Spinner from "./Spinner.svelte";

  interface Props {
    path: string;
  }

  let { path }: Props = $props();

  /** The picked sheet; "" = not chosen yet → the workbook's first sheet.
   *  XlsxView remounts per file (FileView's {#key path}), so this starts empty
   *  for each new spreadsheet. */
  let selected = $state("");
  let sheets = $state<string[]>([]);
  /** The sheet the loaded page actually belongs to (from the server) — also the
   *  "loaded" flag: null while the first probe is in flight. */
  let sheetLoaded = $state<string | null>(null);
  let error = $state<string | null>(null);

  // Probe the selected sheet for the workbook's sheet list + the resolved sheet
  // name (limit 1 — the grid itself is TableView's job). Re-runs on path or
  // sheet change; cleanup cancels an in-flight fetch so a fast switch can't land
  // its result after a newer one. NOTE: the page payload must NOT be handed to
  // TableView — sharing this component's deeply-reactive $state object across the
  // boundary cross-links the two reactive graphs into a freeze. TableView reads
  // its own plain page via `fetchPage`.
  $effect(() => {
    const p = path;
    const target = selected === "" ? null : selected;
    let cancelled = false;
    error = null;
    void fsXlsx(p, target, 0, 1)
      .then((page) => {
        if (cancelled) return;
        sheets = page.sheets;
        sheetLoaded = page.sheet;
      })
      .catch((e) => {
        if (cancelled) return;
        error = e instanceof Error ? e.message : "failed to open the spreadsheet";
      });
    return () => {
      cancelled = true;
    };
  });

  // TableView's page fetcher for the CURRENT sheet. This MUST be a stable
  // reference between renders: an inline `fetchPage={() => …}` recreated every
  // render re-triggers TableView's effect. Derived → it changes only when the
  // sheet (or path) does, which is exactly when the {#key} below remounts.
  const pageFetcher = $derived.by(() => {
    const p = path;
    const s = sheetLoaded ?? "";
    return (offset: number, limit: number): Promise<TablePage> => fsXlsx(p, s, offset, limit);
  });
</script>

<div class="xlsx-view">
  {#if error !== null}
    <div class="file-error">{error}</div>
  {:else if sheetLoaded === null}
    <Spinner />
  {:else}
    {#if sheets.length > 1}
      <div class="sheets" role="tablist" aria-label="sheets">
        {#each sheets as s (s)}
          <button
            class="sheet"
            class:on={s === sheetLoaded}
            role="tab"
            aria-selected={s === sheetLoaded}
            title={s}
            onclick={() => (selected = s)}>{s}</button
          >
        {/each}
      </div>
    {/if}
    <div class="grid">
      {#key sheetLoaded}
        <TableView {path} fetchPage={pageFetcher} />
      {/key}
    </div>
  {/if}
</div>

<style>
  .xlsx-view {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
  }

  /* Sheet tabs — a quiet horizontal strip, matching the mode-toggle treatment.
     Scrolls sideways when a workbook has many sheets. */
  .sheets {
    flex: none;
    display: flex;
    align-items: stretch;
    gap: 1px;
    height: 28px;
    padding: 0 0.4rem;
    border-bottom: 1px solid var(--edge);
    overflow-x: auto;
    scrollbar-width: none;
  }

  .sheets::-webkit-scrollbar {
    display: none;
  }

  .sheet {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: pointer;
    padding: 0 0.7rem;
    max-width: 18ch;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    border-bottom: 2px solid transparent;
    transition:
      color 0.12s ease,
      border-color 0.12s ease;
  }

  .sheet:hover {
    color: var(--fg);
  }

  .sheet.on {
    color: var(--fg);
    border-bottom-color: var(--accent);
  }

  .grid {
    flex: 1;
    position: relative;
    min-height: 0;
  }

  .file-error {
    margin: auto;
    color: var(--muted);
    font-size: var(--text-md);
    padding: 1rem;
    text-align: center;
  }
</style>
