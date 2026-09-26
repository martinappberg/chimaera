<script lang="ts">
  /**
   * A slice of a table: CSV/TSV and the bioinformatics formats through
   * `fs/table` (`#row=a-b`, RFC 7111 — row 1 is the header line when the
   * file has one), spreadsheets through `fs/xlsx` (`#sheet=S&range=A1:F20`).
   * Without a selection, the first rows. One request for exactly the rows
   * shown; the full grid is one click away.
   */
  import { fsTable, fsXlsx, tablePreset, type TablePage } from "../../previews/files";
  import { tableWindow, type EmbedFragment } from "./fragment";

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

  const hasHeader = $derived(kind === "xlsx" || (tablePreset(path)?.header ?? true));
  /** Rows as RFC 7111 / A1 numbers them, from the fragment. */
  const rowSel = $derived.by((): EmbedFragment["rows"] => {
    if (frag.rows !== undefined) return frag.rows;
    const r = frag.range;
    if (r !== undefined && r.r1 !== null) return { start: r.r1, end: r.r2 ?? r.r1 };
    return undefined;
  });
  const win = $derived(tableWindow(rowSel, hasHeader, compact ? TILE_ROWS : PEEK));
  const visibleRows = $derived(Math.min(win.limit, compact ? TILE_ROWS : 12));

  let gen = 0;
  $effect(() => {
    const p = path;
    const k = kind;
    const { offset, limit } = win;
    const sheet = frag.sheet ?? null;
    void version;
    if (!active) return;
    const mine = ++gen;
    error = null;
    const load = k === "xlsx" ? fsXlsx(p, sheet, offset, limit) : fsTable(p, offset, limit);
    load.then(
      (t) => {
        if (mine === gen) page = t;
      },
      (e: unknown) => {
        if (mine === gen) error = e instanceof Error ? e.message : "couldn't read this table";
      },
    );
  });

  /** Column indices shown: the range's columns, else the first MAX_COLS. */
  const cols = $derived.by(() => {
    const n = page?.columns.length ?? 0;
    const r = frag.range;
    const from = r?.c1 != null ? r.c1 - 1 : 0;
    const to = r?.c2 != null ? r.c2 : r?.c1 != null ? r.c1 : n;
    const all = Array.from({ length: Math.max(0, Math.min(n, to) - from) }, (_, i) => from + i);
    return all.slice(0, MAX_COLS);
  });
  const elided = $derived(page !== null && frag.range === undefined && page.columns.length > MAX_COLS);
  /** The first row number shown, as the fragment counts rows. */
  const firstNumber = $derived(win.offset + (hasHeader ? 2 : 1));
  const numbered = $derived(rowSel !== undefined);

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
