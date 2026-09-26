<script lang="ts">
  /**
   * A slice of a table: CSV/TSV and the bioinformatics formats through
   * `fs/table` (`#row=a-b`, `#col=`, `#cell=`, RFC 7111 — row 1 is the
   * header line when the file has one), spreadsheets through `fs/xlsx`
   * (`#sheet=S&range=A1:F20`, A1 counted from the sheet's corner, which the
   * first page's `origin` places on the grid). Without a selection, the
   * first rows. One request for exactly the rows shown (two when a range's
   * sheet does not start at A1); the full grid is one click away.
   */
  import { fsTable, fsXlsx, tableHeaderRow, type TablePage, type XlsxPage } from "../../previews/files";
  import { a1Column } from "../locator";
  import { rangeLabel, rangeOutside, tableSlice, tableWindow, type EmbedFragment } from "./fragment";

  interface Props {
    path: string;
    version: string;
    kind: "table" | "xlsx";
    frag: EmbedFragment;
    compact: boolean;
    active: boolean;
  }

  let { path, version, kind, frag, compact, active }: Props = $props();

  const ROW_H = 22;
  const PEEK = 10;
  const TILE_ROWS = 6;
  /** Columns drawn before eliding (a wide result table stays calm). */
  const MAX_COLS = 12;

  let page = $state<TablePage | null>(null);
  let error = $state<string | null>(null);

  const hasHeader = $derived(kind === "xlsx" || tableHeaderRow(path));
  const peek = $derived(compact ? TILE_ROWS : PEEK);
  const range = $derived(frag.at?.range);
  const sheet = $derived(frag.at?.sheet ?? null);

  /** Where the sheet's used range starts (0-based row, column), as its
   *  first page reported it, for this file version and sheet. Until then
   *  a range is read as if the sheet started at A1 (most do). */
  let learned = $state.raw<{ key: string; origin: readonly [number, number] } | null>(null);
  const sheetKey = $derived(`${path}\u0000${version}\u0000${sheet ?? ""}`);
  const origin = $derived<readonly [number, number] | null>(
    kind === "xlsx" && learned !== null && learned.key === sheetKey ? learned.origin : null,
  );
  /** The selected block: RFC 7111 rows (the header row is row 1), grid columns. */
  const slice = $derived(tableSlice(frag, origin ?? [0, 0]));
  /** A range wholly above or left of the sheet's data: nothing to show. */
  const outside = $derived(range !== undefined && origin !== null && rangeOutside(range, origin));
  const win = $derived(tableWindow(slice, hasHeader, peek));
  // Primitives, so a re-derived but equal window does not refetch.
  const offset = $derived(win.offset);
  const limit = $derived(win.limit);
  const visibleRows = $derived(Math.min(limit, compact ? TILE_ROWS : 12));

  let gen = 0;
  $effect(() => {
    const p = path;
    const k = kind;
    const o = offset;
    const l = limit;
    const s = sheet;
    const key = sheetKey;
    void version;
    if (!active || outside) return;
    const mine = ++gen;
    error = null;
    const load = k === "xlsx" ? fsXlsx(p, s, o, l) : fsTable(p, o, l);
    load.then(
      (t) => {
        if (mine !== gen) return;
        if (k === "xlsx") {
          const at = (t as XlsxPage).origin ?? [0, 0];
          const known = origin;
          if (known === null || known[0] !== at[0] || known[1] !== at[1]) learned = { key, origin: [at[0], at[1]] };
          // Fetched for a range read from A1 while the sheet starts elsewhere:
          // these are other rows. The window moves, and its fetch lands instead.
          const w = tableWindow(tableSlice(frag, at), hasHeader, peek);
          if (w.offset !== o || w.limit !== l) return;
        }
        page = t;
      },
      (e: unknown) => {
        if (mine === gen) error = e instanceof Error ? e.message : "couldn't read this table";
      },
    );
  });

  /** Column indices shown: the slice's columns, else the first MAX_COLS. */
  const cols = $derived.by(() => {
    const n = page?.columns.length ?? 0;
    const c = slice?.col;
    const from = c !== undefined ? c - 1 : 0;
    const to = c !== undefined ? (slice?.endCol ?? c) : n;
    const all = Array.from({ length: Math.max(0, Math.min(n, to) - from) }, (_, i) => from + i);
    return all.slice(0, MAX_COLS);
  });
  const elided = $derived(page !== null && slice?.col === undefined && page.columns.length > MAX_COLS);
  /** The first row number shown: the full grid's own for `row=`/`cell=`
   *  (data rows from 1, so the card and the table it opens agree; the
   *  header's words say the same), the sheet's own for an A1 range, as a
   *  spreadsheet names it. */
  const firstNumber = $derived(
    range !== undefined ? offset + (hasHeader ? 2 : 1) + (origin?.[0] ?? 0) : offset + 1,
  );
  const numbered = $derived(slice?.row !== undefined);
  const outsideNote = $derived(
    outside && range !== undefined && origin !== null
      ? `${rangeLabel(range)} is outside this sheet's data, which starts at ${a1Column(origin[1] + 1)}${origin[0] + 1}`
      : null,
  );

  const footer = $derived.by(() => {
    if (page === null) return "";
    const total = page.total_rows ?? null;
    const est = page.est_rows ?? null;
    const of =
      total !== null
        ? `${total.toLocaleString()} rows`
        : est !== null
          ? `~${Math.round(est).toLocaleString()} rows`
          : page.truncated
            ? "more rows"
            : `${page.rows.length} rows`;
    return numbered ? `${page.rows.length} shown · ${of}` : `first ${page.rows.length} · ${of}`;
  });
</script>

<div class="table-body" class:tile={compact} style:--rows={visibleRows} style:--row-h="{ROW_H}px">
  {#if error !== null}
    <div class="note">{error}</div>
  {:else if outsideNote !== null}
    <div class="note">{outsideNote}</div>
  {:else}
    <div class="scroll">
      {#if page !== null}
        <table>
          <thead>
            <tr>
              {#if numbered}<th class="num"></th>{/if}
              {#each cols as c (c)}
                <th title={page.columns[c]}>{page.columns[c]}</th>
              {/each}
              {#if elided}<th class="elide">… {page.columns.length - MAX_COLS} more</th>{/if}
            </tr>
          </thead>
          <tbody>
            {#each page.rows as row, r (r)}
              <tr>
                {#if numbered}<td class="num">{firstNumber + r}</td>{/if}
                {#each cols as c (c)}
                  <td title={row[c] ?? ""}>{row[c] ?? ""}</td>
                {/each}
                {#if elided}<td class="elide">…</td>{/if}
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </div>
    <div class="foot">{footer}</div>
  {/if}
</div>

<style>
  .table-body {
    display: flex;
    flex-direction: column;
  }
  .table-body.tile {
    flex: 1;
    min-height: 0;
  }
  .scroll {
    /* Header + rows, each a row height plus its 1px rule, and the padding:
       the whole slice shows without a scrollbar up to 12 rows. */
    height: calc((var(--rows) + 1) * (var(--row-h) + 1px) + 10px);
    overflow: auto;
    scrollbar-width: thin;
    padding: 4px 8px;
  }
  .tile .scroll {
    flex: 1;
    height: auto;
    min-height: 0;
  }
  table {
    border-collapse: collapse;
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  th,
  td {
    height: var(--row-h);
    max-width: 200px;
    padding: 0 8px;
    border-bottom: 1px solid color-mix(in srgb, var(--edge) 55%, transparent);
    overflow: hidden;
    text-overflow: ellipsis;
    text-align: left;
  }
  th {
    position: sticky;
    top: 0;
    background: color-mix(in srgb, var(--fg) 3%, var(--bg));
    color: var(--muted);
    font-weight: 600;
    border-bottom-color: var(--edge);
  }
  tbody tr:hover td {
    background: var(--row-hover);
  }
  .num {
    color: color-mix(in srgb, var(--muted) 75%, transparent);
    text-align: right;
    padding-right: 10px;
  }
  .elide {
    color: var(--muted);
    font-weight: 400;
  }
  .foot {
    padding: 3px 10px 5px;
    border-top: 1px solid color-mix(in srgb, var(--edge) 40%, transparent);
    color: var(--muted);
    font-size: var(--text-xs);
    min-height: 1.6em;
  }
  .note {
    padding: 12px;
    color: var(--muted);
    font-size: var(--text-xs);
  }
</style>
