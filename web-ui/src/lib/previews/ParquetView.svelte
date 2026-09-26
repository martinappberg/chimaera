<script lang="ts">
  /**
   * A Parquet file as a paged grid, read in the browser over ranged `/raw`
   * requests (`parquet.ts`): the footer first (row count, schema, codecs from
   * the metadata), then only the column-chunk bytes under the rows on screen.
   * The daemon streams byte ranges and holds nothing. The grid is the shared
   * `TableView` driven through its `fetchPage` override (never a `$state` page
   * object across the boundary — see XlsxView), so paging, virtualization,
   * jump to row, cell expand and copy all come with it. A schema tab lists
   * every column with its logical type, and the bar says how much of the file
   * has crossed the network so far.
   */
  import { basename, fsDownload, humanSize, type TablePage } from "./files";
  import { isRemoteHost } from "../net/api";
  import { formatCell, pageBounds } from "./parquetGrid";
  import { describeType, ParquetError, ParquetSource } from "./parquet";
  import type { SchemaTree } from "hyparquet";
  import TableView from "./TableView.svelte";
  import Spinner from "./Spinner.svelte";

  interface Props {
    path: string;
  }

  let { path }: Props = $props();

  // Not deeply reactive: a class instance holding caches and in-flight reads.
  let source = $state.raw<ParquetSource | null>(null);
  let error = $state<string | null>(null);
  let mode = $state<"data" | "schema">("data");
  /** Bumped after each read so the traffic figure re-reads. */
  let reads = $state(0);

  $effect(() => {
    const p = path;
    let stale = false;
    source = null;
    error = null;
    ParquetSource.open(p).then(
      (s) => {
        if (!stale) source = s;
      },
      (e: unknown) => {
        if (!stale) error = e instanceof Error ? e.message : "the file could not be opened";
      },
    );
    return () => {
      stale = true;
    };
  });

  const unsupported = $derived(source?.unsupportedCodecs() ?? []);

  // Stable per source (TableView's effect keys on the reference).
  const pageFetcher = $derived.by(() => {
    const s = source;
    if (s === null) return undefined;
    const names = s.columns.map((c) => c.name);
    const kinds = s.columns.map((c) => c.kind);
    return async (offset: number, limit: number): Promise<TablePage> => {
      const total = s.facts.rows;
      const { start, end } = pageBounds(offset, limit, total);
      try {
        const raw = await s.read(start, end);
        return {
          columns: names,
          rows: raw.map((r) => r.map((v, i) => formatCell(v, kinds[i]))),
          offset: start,
          truncated: end < total,
          total_rows: total,
        };
      } catch (e) {
        throw new Error(e instanceof ParquetError ? e.message : "rows could not be read");
      } finally {
        reads += 1;
      }
    };
  });

  const traffic = $derived.by(() => {
    void reads;
    return source?.traffic ?? null;
  });

  const facts = $derived(source?.facts ?? null);

  /** Schema rows: every node below the root, indented by depth. */
  interface SchemaRow {
    depth: number;
    name: string;
    type: string;
  }
  const schemaRows = $derived.by<SchemaRow[]>(() => {
    const s = source;
    if (s === null) return [];
    const out: SchemaRow[] = [];
    const walk = (node: SchemaTree, depth: number) => {
      for (const c of node.children) {
        out.push({ depth, name: c.element.name, type: describeType(c) });
        if (out.length < 2000) walk(c, depth + 1);
      }
    };
    walk(s.schema, 0);
    return out;
  });

  const remote = isRemoteHost();
  let downloadError = $state<string | null>(null);
  async function download(): Promise<void> {
    downloadError = null;
    try {
      await fsDownload(path);
    } catch (e) {
      downloadError = e instanceof Error ? e.message : "download failed";
    }
  }

  const count = (n: number, one: string, many: string) => `${n.toLocaleString("en-US")} ${n === 1 ? one : many}`;
</script>

<div class="pq-view">
  <div class="pq-bar">
    {#if facts !== null && source !== null}
      <span class="facts" title={facts.createdBy ?? undefined}>
        {count(facts.rows, "row", "rows")} · {count(source.columns.length, "column", "columns")} ·
        {count(facts.rowGroups, "row group", "row groups")} · {facts.codecs.map((c) => c.toLowerCase()).join(", ")}
      </span>
      {#if traffic !== null}
        <span
          class="traffic"
          title="bytes this view has read over the network ({traffic.requests} ranged requests); the daemon holds none of it"
          >read {humanSize(traffic.bytes)} of {humanSize(facts.size)}</span
        >
      {/if}
    {/if}
    {#if downloadError !== null}<span class="bar-err">{downloadError}</span>{/if}
    <span class="spacer"></span>
    {#if remote}
      <button class="bbtn" onclick={() => void download()} title="download {basename(path)} to this computer">download</button>
    {/if}
    {#if source !== null}
      <div class="switch" role="tablist" aria-label="parquet view">
        <button class="seg" class:on={mode === "data"} role="tab" aria-selected={mode === "data"} onclick={() => (mode = "data")}>data</button>
        <button class="seg" class:on={mode === "schema"} role="tab" aria-selected={mode === "schema"} onclick={() => (mode = "schema")}
          >schema</button
        >
      </div>
    {/if}
  </div>

  <div class="pq-body">
    {#if error !== null}
      <div class="pq-msg">
        <span>{error}</span>
      </div>
    {:else if source === null}
      <Spinner label="reading the footer" />
    {:else if mode === "schema"}
      <div class="schema" role="region" aria-label="schema">
        <table>
          <thead><tr><th class="num">#</th><th>column</th><th>type</th></tr></thead>
          <tbody>
            {#each schemaRows as r, i (i)}
              <tr class:nested={r.depth > 0}>
                <td class="num">{r.depth === 0 ? schemaRows.slice(0, i + 1).filter((x) => x.depth === 0).length : ""}</td>
                <td class="name" style:padding-left="{0.6 + r.depth * 1.1}rem">{r.name}</td>
                <td class="type">{r.type}</td>
              </tr>
            {/each}
          </tbody>
        </table>
        {#if facts !== null}
          <dl class="file-facts">
            <dt>rows</dt><dd>{facts.rows.toLocaleString("en-US")}</dd>
            <dt>row groups</dt><dd>{facts.rowGroups.toLocaleString("en-US")}</dd>
            <dt>size</dt><dd>{humanSize(facts.size)}</dd>
            <dt>compression</dt><dd>{facts.codecs.join(", ")}</dd>
            <dt>page index</dt><dd>{facts.pageIndex ? "yes" : "no — pages are found by reading their headers"}</dd>
            {#if facts.createdBy !== null}<dt>written by</dt><dd>{facts.createdBy}</dd>{/if}
            {#if facts.keyValueKeys.length > 0}<dt>metadata keys</dt><dd>{facts.keyValueKeys.join(", ")}</dd>{/if}
          </dl>
        {/if}
      </div>
    {:else if unsupported.length > 0}
      <div class="pq-msg">
        <span>{unsupported.join(", ")} compression isn't supported here</span>
        <span class="hint">the schema tab still shows the columns{remote ? "; download the file to read it locally" : ""}</span>
      </div>
    {:else if pageFetcher !== undefined}
      {#key source}
        <TableView {path} fetchPage={pageFetcher} />
      {/key}
    {/if}
  </div>
</div>

<style>
  .pq-view {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
  }

  .pq-bar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.6rem;
    height: 26px;
    padding: 0 0.5rem 0 0.7rem;
    border-bottom: 1px solid var(--edge);
    font-size: var(--text-xs);
    color: var(--muted);
    min-width: 0;
  }

  .facts,
  .traffic,
  .bar-err {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .facts {
    color: var(--fg);
    font-variant-numeric: tabular-nums;
  }

  .traffic {
    font-variant-numeric: tabular-nums;
  }

  .bar-err {
    color: var(--err);
  }

  .spacer {
    flex: 1;
  }

  .switch {
    display: flex;
    align-items: center;
    gap: 1px;
  }

  .seg,
  .bbtn {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: pointer;
    padding: 2px 8px;
    border-radius: 4px;
    white-space: nowrap;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .seg {
    letter-spacing: 0.04em;
  }

  .seg:hover,
  .bbtn:hover {
    color: var(--fg);
  }

  .bbtn:hover {
    background: var(--row-hover);
  }

  .seg.on {
    color: var(--fg);
    background: var(--row-active);
  }

  .pq-body {
    position: relative;
    flex: 1;
    min-height: 0;
  }

  .pq-msg {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 0.4rem;
    padding: 1rem;
    color: var(--muted);
    font-size: var(--text-md);
    text-align: center;
  }

  .hint {
    font-size: var(--text-xs);
    opacity: 0.85;
  }

  .schema {
    position: absolute;
    inset: 0;
    overflow: auto;
    scrollbar-width: thin;
    padding: 0.8rem 1rem 1.5rem;
    font-size: var(--text-sm);
  }

  table {
    border-collapse: collapse;
    min-width: min(640px, 100%);
    font-variant-numeric: tabular-nums;
  }

  th {
    text-align: left;
    font-weight: 500;
    color: var(--muted);
    font-size: var(--text-xs);
    letter-spacing: 0.03em;
    padding: 0.3rem 0.6rem;
    border-bottom: 1px solid var(--edge);
  }

  td {
    padding: 0.28rem 0.6rem;
    border-bottom: 1px solid color-mix(in srgb, var(--edge) 60%, transparent);
    vertical-align: top;
  }

  .num {
    width: 3ch;
    text-align: right;
    color: var(--muted);
    font-family: var(--mono);
    font-size: var(--text-xs);
  }

  .name {
    font-family: var(--mono);
    color: var(--fg);
  }

  tr.nested .name {
    color: var(--muted);
  }

  .type {
    font-family: var(--mono);
    color: var(--syn-type);
  }

  .file-facts {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 0.3rem 1rem;
    margin: 1.2rem 0 0;
    font-size: var(--text-xs);
  }

  .file-facts dt {
    color: var(--muted);
  }

  .file-facts dd {
    margin: 0;
    color: var(--fg);
    overflow-wrap: anywhere;
  }

  @media (prefers-reduced-motion: reduce) {
    .seg,
    .bbtn {
      transition: none;
    }
  }
</style>
