<script lang="ts">
  /**
   * BoardView's outline rail + numeric inspector (geometry + chart config).
   * Purely presentational — plain parsed data and callbacks only, no shared
   * state; every mutation goes back through the parent's commit path.
   */
  import { chartConfig, MARK_SWAP_KINDS, SORT_OPTIONS, type ObjInfo } from "./boardInteract";

  interface Props {
    title: string;
    objects: ObjInfo[];
    selected: string | null;
    /** The board theme's categorical ramp (@token + resolved hex), from the
     *  render response — the series-color swatches. */
    catSwatches: { token: string; hex: string }[];
    onselect: (id: string | null) => void;
    oncommitfield: (field: "x" | "y" | "w" | "h", raw: string) => void;
    /** Sparse config edit on the selected object (the /board/edit set op):
     *  dot-path → value, null clears. */
    oncommitset: (set: Record<string, unknown>) => void;
  }
  let { title, objects, selected, catSwatches, onselect, oncommitfield, oncommitset }: Props =
    $props();

  const selectedObj = $derived(objects.find((o) => o.id === selected) ?? null);

  // The chart config projection of the selected object (null for non-charts):
  // current values are the file's own literals, refreshed by the same reparse
  // that moves the geometry fields.
  const chart = $derived(selectedObj !== null ? chartConfig(selectedObj) : null);

  // A sort value outside the canonical set (e.g. a field name) stays visible
  // and selectable — the select must never silently rewrite what the file
  // says just by rendering it.
  const sortOptions = $derived.by(() => {
    const c = chart;
    if (c === null || SORT_OPTIONS.some((o) => o.value === c.sort)) return SORT_OPTIONS;
    return [{ value: c.sort, label: c.sort }, ...SORT_OPTIONS];
  });

  function commitTitle(channel: "x" | "y", raw: string): void {
    const c = chart;
    const cur = channel === "x" ? c?.x?.title : c?.y?.title;
    if (c === null || cur === undefined) return;
    const v = raw.trim();
    if (v === cur) return;
    oncommitset({ [`${channel}.title`]: v === "" ? null : v });
  }

  function commitSort(value: string): void {
    const c = chart;
    if (c === null || c.sortChannel === null || value === c.sort) return;
    oncommitset({ [`${c.sortChannel}.sort`]: value === "" ? null : value });
  }

  function commitMarkKind(value: string): void {
    const c = chart;
    if (c === null || value === c.markKind) return;
    oncommitset({ "marks.0.mark": value });
  }

  /** Sets the single mark's `fill` — the token series_color resolves first —
   *  or clears it back to the theme's default series color. */
  function commitMarkColor(token: string | null): void {
    const c = chart;
    if (c === null || (token ?? "") === c.markColor) return;
    oncommitset({ "marks.0.fill": token });
  }
</script>

<aside class="rail">
  <div class="rail-title">{title}</div>
  <div class="outline">
    {#each objects as o (o.id)}
      <button
        class="obj"
        class:on={o.id === selected}
        onclick={() => onselect(o.id === selected ? null : o.id)}
      >
        <span class="obj-kind">{o.kind}</span>
        <span class="obj-id">{o.id}</span>
      </button>
    {/each}
    {#if objects.length === 0}
      <div class="empty">no objects on this page</div>
    {/if}
  </div>

  {#if selectedObj !== null && selectedObj.at !== null && selectedObj.size !== null}
    <div class="inspector">
      <div class="insp-head">{selectedObj.id}</div>
      <div class="insp-grid">
        <label>x <input type="number" step="8" value={selectedObj.at[0]}
          onchange={(e) => oncommitfield("x", (e.currentTarget as HTMLInputElement).value)} /></label>
        <label>y <input type="number" step="8" value={selectedObj.at[1]}
          onchange={(e) => oncommitfield("y", (e.currentTarget as HTMLInputElement).value)} /></label>
        <label>w <input type="number" step="8" value={selectedObj.size[0]}
          onchange={(e) => oncommitfield("w", (e.currentTarget as HTMLInputElement).value)} /></label>
        <label>h <input type="number" step="8" value={selectedObj.size[1]}
          onchange={(e) => oncommitfield("h", (e.currentTarget as HTMLInputElement).value)} /></label>
      </div>
      <div class="insp-unit">pt · snaps to the 8 pt grid</div>
      {#if chart !== null}
        <!-- Chart configuration: a chart is one declarative object; its
             axes/sort/marks are config the engine lays out, edited here as
             sparse fields via the /board/edit set op — never client layout. -->
        <div class="insp-sect">chart</div>
        {#if chart.x !== null}
          <label class="insp-row">x label
            <input type="text" value={chart.x.title} placeholder="none" spellcheck="false"
              onchange={(e) => commitTitle("x", (e.currentTarget as HTMLInputElement).value)} /></label>
        {/if}
        {#if chart.y !== null}
          <label class="insp-row">y label
            <input type="text" value={chart.y.title} placeholder="none" spellcheck="false"
              onchange={(e) => commitTitle("y", (e.currentTarget as HTMLInputElement).value)} /></label>
        {/if}
        {#if chart.sortChannel !== null}
          <label class="insp-row">sort
            <select value={chart.sort}
              onchange={(e) => commitSort((e.currentTarget as HTMLSelectElement).value)}>
              {#each sortOptions as s (s.value)}
                <option value={s.value}>{s.label}</option>
              {/each}
            </select></label>
        {/if}
        {#if chart.markSwappable}
          <label class="insp-row">mark
            <select value={chart.markKind}
              onchange={(e) => commitMarkKind((e.currentTarget as HTMLSelectElement).value)}>
              {#each MARK_SWAP_KINDS as k (k)}
                <option value={k}>{k}</option>
              {/each}
            </select></label>
        {/if}
        {#if chart.markCount === 1 && catSwatches.length > 0}
          <div class="insp-row swatch-row" role="group" aria-label="series color (theme tokens)">
            <span class="swatch-label">color</span>
            <button
              class="swatch auto"
              class:on={chart.markColor === ""}
              title="theme default"
              aria-label="series color: theme default"
              onclick={() => commitMarkColor(null)}
            >–</button>
            {#each catSwatches as s (s.token)}
              <button
                class="swatch"
                class:on={chart.markColor === s.token}
                style:background={s.hex}
                title={s.token}
                aria-label={`series color ${s.token}`}
                onclick={() => commitMarkColor(s.token)}
              ></button>
            {/each}
          </div>
        {/if}
      {/if}
    </div>
  {/if}
</aside>

<style>
  .rail {
    width: 220px;
    flex-shrink: 0;
    border-left: 1px solid var(--edge);
    background: var(--rail-bg);
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  .rail-title {
    padding: 10px 12px 6px;
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--fg);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .outline {
    flex: 1;
    overflow-y: auto;
    padding: 0 6px;
  }
  .obj {
    display: flex;
    align-items: baseline;
    gap: 6px;
    width: 100%;
    padding: 4px 6px;
    background: none;
    border: none;
    border-radius: 4px;
    cursor: pointer;
    text-align: left;
    font-size: var(--text-xs);
  }
  .obj:hover {
    background: var(--row-hover);
  }
  .obj.on {
    background: var(--row-active);
  }
  .obj-kind {
    color: var(--muted);
    font-family: var(--mono);
    flex-shrink: 0;
  }
  .obj-id {
    color: var(--fg);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .empty {
    color: var(--muted);
    font-size: var(--text-xs);
    padding: 8px 6px;
  }
  .inspector {
    border-top: 1px solid var(--edge);
    padding: 8px 12px 10px;
  }
  .insp-head {
    font-size: var(--text-xs);
    font-family: var(--mono);
    color: var(--accent);
    margin-bottom: 6px;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .insp-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 4px 8px;
  }
  .insp-grid label {
    display: flex;
    align-items: center;
    gap: 4px;
    font-size: var(--text-xs);
    color: var(--muted);
    font-family: var(--mono);
  }
  .insp-grid input {
    width: 100%;
    min-width: 0;
    background: var(--term-bg);
    border: 1px solid var(--edge);
    border-radius: 3px;
    color: var(--fg);
    font-size: var(--text-xs);
    font-family: var(--mono);
    padding: 2px 4px;
  }
  .insp-unit {
    margin-top: 6px;
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .insp-sect {
    margin-top: 10px;
    margin-bottom: 4px;
    font-size: var(--text-xs);
    font-family: var(--mono);
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  .insp-row {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-top: 4px;
    font-size: var(--text-xs);
    color: var(--muted);
    font-family: var(--mono);
    white-space: nowrap;
  }
  .insp-row input,
  .insp-row select {
    flex: 1;
    width: 100%;
    min-width: 0;
    background: var(--term-bg);
    border: 1px solid var(--edge);
    border-radius: 3px;
    color: var(--fg);
    font-size: var(--text-xs);
    font-family: var(--mono);
    padding: 2px 4px;
  }
  .swatch-row {
    flex-wrap: wrap;
  }
  .swatch-label {
    flex-shrink: 0;
  }
  .swatch {
    width: 16px;
    height: 16px;
    padding: 0;
    flex: none;
    border: 1px solid var(--edge);
    border-radius: 3px;
    cursor: pointer;
  }
  .swatch.auto {
    background: var(--term-bg);
    color: var(--muted);
    font-size: var(--text-xs);
    line-height: 1;
  }
  .swatch.on {
    box-shadow: 0 0 0 2px color-mix(in srgb, var(--accent) 60%, transparent);
    border-color: var(--accent);
  }
  .swatch:hover {
    border-color: var(--accent);
  }
</style>
