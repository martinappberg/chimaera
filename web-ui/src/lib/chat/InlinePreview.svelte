<script lang="ts">
  import {
    basename,
    fsTable,
    rawTicketUrl,
    viewKindFor,
    type TablePage,
  } from "../previews/files";
  import FileIcon from "../shared/FileIcon.svelte";

  /**
   * Inline artifact preview under a tool card: the output IS the point of
   * many jobs (a plot, a results table, a report), so it shows itself in
   * the conversation. Deliberately small — a glance, not a viewer; one
   * click opens the real thing in a native pane. Kinds: image (ticketed
   * /raw/ URL), table (first rows via fs/table), pdf (embedded /raw/).
   */
  interface Props {
    path: string;
    onOpen?: (path: string) => void;
  }

  let { path, onOpen }: Props = $props();

  const kind = $derived(viewKindFor(path));
  /** Rows shown inline; enough to see the shape, never the whole file. */
  const TABLE_PEEK_ROWS = 5;
  /** Columns shown inline before eliding (wide result tables stay calm). */
  const TABLE_PEEK_COLS = 8;

  let rawUrl = $state<string | null>(null);
  let table = $state<TablePage | null>(null);

  $effect(() => {
    rawUrl = null;
    table = null;
    let stale = false;
    if (kind === "image" || kind === "pdf") {
      void rawTicketUrl(path)
        .then((url) => {
          if (!stale) rawUrl = url;
        })
        .catch(() => {});
    } else if (kind === "table") {
      void fsTable(path, 0, TABLE_PEEK_ROWS)
        .then((page) => {
          if (!stale) table = page;
        })
        .catch(() => {});
    }
    return () => {
      stale = true;
    };
  });

  const colsElided = $derived(table !== null && table.columns.length > TABLE_PEEK_COLS);
  const shownCols = $derived(table?.columns.slice(0, TABLE_PEEK_COLS) ?? []);
</script>

{#if kind === "image" && rawUrl !== null}
  <button class="img-preview" title="open in a pane" onclick={() => onOpen?.(path)}>
    <img src={rawUrl} alt={basename(path)} />
  </button>
{:else if kind === "table" && table !== null}
  <div class="artifact">
    <button class="artifact-head" title="open the full table in a pane" onclick={() => onOpen?.(path)}>
      <FileIcon {path} size={13} />
      <span class="artifact-name">{basename(path)}</span>
      <span class="artifact-hint">
        {table.truncated ? `first ${table.rows.length} rows — open full table` : `${table.rows.length} rows`}
      </span>
    </button>
    <div class="tbl-scroll">
      <table>
        <thead>
          <tr>
            {#each shownCols as col (col)}
              <th>{col}</th>
            {/each}
            {#if colsElided}
              <th class="elide">… {table.columns.length - TABLE_PEEK_COLS} more</th>
            {/if}
          </tr>
        </thead>
        <tbody>
          {#each table.rows as row, r (r)}
            <tr>
              {#each shownCols as _, c (c)}
                <td>{row[c] ?? ""}</td>
              {/each}
              {#if colsElided}
                <td class="elide">…</td>
              {/if}
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  </div>
{:else if kind === "pdf" && rawUrl !== null}
  <div class="artifact">
    <button class="artifact-head" title="open in a pane" onclick={() => onOpen?.(path)}>
      <FileIcon {path} size={13} />
      <span class="artifact-name">{basename(path)}</span>
      <span class="artifact-hint">open in a pane</span>
    </button>
    <object class="pdf" data={rawUrl} type="application/pdf" aria-label={basename(path)}>
      <span class="pdf-fallback">PDF preview unavailable here — open it in a pane</span>
    </object>
  </div>
{/if}

<style>
  .img-preview {
    display: block;
    width: 100%;
    border: none;
    border-top: 1px solid var(--edge);
    background: color-mix(in srgb, var(--fg) 3%, transparent);
    padding: 8px;
    cursor: zoom-in;
    text-align: center;
  }
  .img-preview img {
    max-width: 100%;
    max-height: 240px;
    object-fit: contain;
    border-radius: 4px;
  }
  .artifact {
    border-top: 1px solid var(--edge);
  }
  .artifact-head {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    padding: 5px 10px;
    background: none;
    border: none;
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
    text-align: left;
    cursor: pointer;
    transition: background-color 0.12s ease;
  }
  .artifact-head:hover {
    background: color-mix(in srgb, var(--fg) 4%, transparent);
  }
  .artifact-name {
    font-family: var(--mono, monospace);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .artifact-hint {
    margin-left: auto;
    flex: none;
    color: var(--muted);
    font-size: var(--text-xs);
  }
  .tbl-scroll {
    overflow-x: auto;
    scrollbar-width: thin;
    padding: 0 10px 8px;
  }
  table {
    border-collapse: collapse;
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  th,
  td {
    border: 1px solid color-mix(in srgb, var(--edge) 70%, transparent);
    padding: 2px 8px;
    text-align: left;
    max-width: 180px;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  th {
    color: var(--muted);
    font-weight: 600;
  }
  .elide {
    color: var(--muted);
    font-weight: 400;
  }
  .pdf {
    display: block;
    width: 100%;
    height: 300px;
    border: none;
    background: color-mix(in srgb, var(--fg) 3%, transparent);
  }
  .pdf-fallback {
    display: block;
    padding: 10px;
    color: var(--muted);
    font-size: var(--text-sm);
  }
</style>
