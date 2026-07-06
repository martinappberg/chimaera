<script lang="ts">
  import { fsTable, type TablePage } from "./files";

  interface Props {
    path: string;
  }

  let { path }: Props = $props();

  const PAGE_ROWS = 200;

  let page = $state<TablePage | null>(null);
  let error = $state<string | null>(null);
  let loading = $state(false);
  let offset = $state(0);
  let scroller = $state<HTMLDivElement | null>(null);

  $effect(() => {
    const p = path;
    offset = 0;
    page = null;
    error = null;
    void load(p, 0);
  });

  async function load(p: string, off: number): Promise<void> {
    loading = true;
    try {
      const next = await fsTable(p, off, PAGE_ROWS);
      if (p !== path) return; // tab switched mid-flight
      page = next;
      offset = next.offset;
      error = null;
      scroller?.scrollTo({ top: 0 });
    } catch (e) {
      if (p !== path) return;
      error = e instanceof Error ? e.message : "failed to load table";
      page = null;
    } finally {
      if (p === path) loading = false;
    }
  }

  const hasPrev = $derived(offset > 0);
  const hasNext = $derived(page?.truncated === true);
  const rowRange = $derived.by(() => {
    if (page === null || page.rows.length === 0) return "no rows";
    const first = offset + 1;
    const last = offset + page.rows.length;
    return `rows ${first.toLocaleString()}–${last.toLocaleString()}${page.truncated ? " of more" : ""}`;
  });

  function prev(): void {
    void load(path, Math.max(0, offset - PAGE_ROWS));
  }

  function next(): void {
    if (page !== null) void load(path, offset + page.rows.length);
  }
</script>

<div class="table-view">
  {#if error !== null}
    <div class="file-error">{error}</div>
  {:else if page !== null}
    <div class="scroll" bind:this={scroller}>
      <table>
        <thead>
          <tr>
            <th class="ln" aria-label="row number"></th>
            {#each page.columns as col, i (i)}
              <th>{col}</th>
            {/each}
          </tr>
        </thead>
        <tbody>
          {#each page.rows as row, r (offset + r)}
            <tr>
              <td class="ln">{(offset + r + 1).toLocaleString()}</td>
              {#each row as cell, c (c)}
                <td>{cell}</td>
              {/each}
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
    <footer class="pager">
      <span class="range">{rowRange}</span>
      <span class="spacer"></span>
      {#if hasPrev || hasNext}
        <button class="pg" disabled={!hasPrev || loading} onclick={prev}>‹ prev</button>
        <button class="pg" disabled={!hasNext || loading} onclick={next}>next ›</button>
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
    z-index: 1;
    background: var(--term-bg);
    text-align: left;
    font-weight: 600;
    color: var(--fg);
    padding: 0.45rem 0.9rem 0.4rem 0.6rem;
    border-bottom: 1px solid var(--edge);
    white-space: nowrap;
  }

  td {
    padding: 0.22rem 0.9rem 0.22rem 0.6rem;
    color: var(--fg);
    white-space: nowrap;
    max-width: 40ch;
    overflow: hidden;
    text-overflow: ellipsis;
    border-bottom: 1px solid color-mix(in srgb, var(--edge) 45%, transparent);
  }

  .ln {
    color: var(--muted);
    opacity: 0.65;
    text-align: right;
    padding-left: 0.9rem;
    user-select: none;
    font-size: 0.66rem;
  }

  tbody tr:hover td {
    background: color-mix(in srgb, var(--fg) 3.5%, transparent);
  }

  .pager {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.4rem;
    height: 28px;
    padding: 0 0.7rem;
    border-top: 1px solid var(--edge);
    font-size: 0.68rem;
    color: var(--muted);
  }

  .range {
    font-variant-numeric: tabular-nums;
  }

  .spacer {
    flex: 1;
  }

  .pg {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: 0.68rem;
    color: var(--muted);
    cursor: pointer;
    padding: 0.1rem 0.4rem;
    border-radius: 4px;
  }

  .pg:hover:not(:disabled) {
    background: var(--row-hover);
    color: var(--fg);
  }

  .pg:disabled {
    opacity: 0.35;
    cursor: default;
  }

  .file-error {
    margin: auto;
    color: var(--muted);
    font-size: 0.8rem;
    padding: 1rem;
    text-align: center;
  }
</style>
