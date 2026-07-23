<script lang="ts">
  /**
   * BoardView's outline rail + numeric inspector (geometry + chart config).
   * Purely presentational — plain parsed data and callbacks only, no shared
   * state; every mutation goes back through the parent's commit path.
   */
  import {
    chartConfig,
    MARK_SWAP_KINDS,
    SORT_OPTIONS,
    type ChildFrame,
    type ObjInfo,
  } from "./boardInteract";
  import { buildLayerTree, selectionBranch, type LayerNode } from "./boardLayers";

  interface Props {
    title: string;
    objects: ObjInfo[];
    selected: string | null;
    /** Composite id → derived children + laid-out frames (the render
     *  response's childFrames) — the composite branch of the layer tree. */
    childFrames: Record<string, ChildFrame[]>;
    /** The drilled-into child's derived id, highlighted in the tree. */
    selectedChild: string | null;
    /** The board theme's categorical ramp (@token + resolved hex), from the
     *  render response — the series-color swatches. */
    catSwatches: { token: string; hex: string }[];
    /** The theme's ground tones (@bg, @surface, …) + resolved hex, from the
     *  render response — the theme-ground swatches (plus white/black, which
     *  are literal grounds this control always offers). */
    bgSwatches: { token: string; hex: string }[];
    /** The theme picker's scheme families (id/label/variant), from the render
     *  response. Empty (or `oncommittheme` absent) → no scheme overrides. */
    schemes?: { id: string; label: string; variant: string }[];
    /** What the board's theme reference selects: `"auto"` (match the app —
     *  the default), a scheme id, or `"pinned"`. Drives the picker's state. */
    themeSelection?: string;
    /** Render diagnostics; the non-info ones surface as the collapsible lint
     *  notes (hidden until the user expands them). */
    diagnostics?: { severity: string; object: string | null; message: string; rendered: string }[];
    /** The file's own `canvas.background` (@token or #hex), null when the
     *  board follows the theme's ground. */
    canvasBackground: string | null;
    /** Board-level commit: set `canvas.background` to a token/#hex, or null to
     *  match the theme again. */
    oncommitcanvas: (background: string | null) => void;
    /** Board-level commit: set the board `theme` to a scheme id, or null to
     *  clear back to `auto` (match the app). Optional — when absent the scheme
     *  row hides (BoardView wires it: `oncommittheme={commitTheme}`). */
    oncommittheme?: (theme: string | null) => void;
    onselect: (id: string | null) => void;
    /** A composite child's click: select the derived child under its composite
     *  (null collapses back to the composite itself). */
    onselectchild: (parentId: string, childId: string | null) => void;
    oncommitfield: (field: "x" | "y" | "w" | "h", raw: string) => void;
    /** Sparse config edit on the selected object (the /board/edit set op):
     *  dot-path → value, null clears. */
    oncommitset: (set: Record<string, unknown>) => void;
  }
  let {
    title,
    objects,
    selected,
    childFrames,
    selectedChild,
    catSwatches,
    bgSwatches,
    schemes = [],
    themeSelection = "auto",
    diagnostics = [],
    canvasBackground,
    oncommitcanvas,
    oncommittheme,
    onselect,
    onselectchild,
    oncommitfield,
    oncommitset,
  }: Props = $props();

  const selectedObj = $derived(objects.find((o) => o.id === selected) ?? null);

  // --- the layer tree -----------------------------------------------------
  /** Top-level objects, group descendants, and composite children folded into
   *  one indented outline (pure — see boardLayers.ts). */
  const tree = $derived(buildLayerTree(objects, childFrames));
  /** Ids to auto-open so the current selection's branch is revealed. */
  const autoOpen = $derived(selectionBranch(tree, selected, selectedChild));

  /** Manually toggled disclosures; absent = follow the selection. */
  let expanded = $state<Record<string, boolean>>({});
  function isOpen(id: string): boolean {
    return expanded[id] ?? autoOpen.has(id);
  }

  /** Whether a row wears the active-selection highlight. A group descendant
   *  never does — its click selects the enclosing group, which is the row that
   *  lights up. */
  function rowActive(node: LayerNode): boolean {
    const s = node.select;
    if (s.via === "child") return selectedChild === node.id;
    if (node.id === s.id) return selected === node.id && selectedChild === null;
    return false;
  }

  /** Route a row click to the callback BoardView exposes for its kind. */
  function selectNode(node: LayerNode): void {
    const s = node.select;
    if (s.via === "child") {
      onselectchild(s.parent, s.id === selectedChild ? null : s.id);
    } else if (node.id === s.id) {
      onselect(s.id === selected ? null : s.id);
    } else {
      // A group descendant: always select the enclosing top-level group.
      onselect(s.id);
    }
  }

  // --- theme picker: ground overrides -------------------------------------
  /** Literal grounds the picker always offers beside the theme's own tones. */
  const GROUNDS: { value: string; label: string }[] = [
    { value: "#ffffff", label: "white" },
    { value: "#000000", label: "black" },
  ];
  /** The file's literal ground, case-folded for the active-swatch check. */
  const activeGround = $derived(canvasBackground?.toLowerCase() ?? null);
  /** The theme picker's caption for the current state (why it looks how it
   *  looks) — the default reads as automatic, never a forced choice. */
  const themeNote = $derived(
    themeSelection === "auto"
      ? "default — matches the app's light/dark"
      : themeSelection === "pinned"
        ? "pinned — ignores the app's mode"
        : "a scheme, still following the app's mode",
  );

  // --- lint notes: collapsed by default -----------------------------------
  const lintNotes = $derived(diagnostics.filter((d) => d.severity !== "info"));
  let lintOpen = $state(false);

  /** The selected child's live frame, for the read-only inspector line. */
  const selectedChildFrame = $derived.by<ChildFrame | null>(() => {
    if (selected === null || selectedChild === null) return null;
    return (childFrames[selected] ?? []).find((c) => c.id === selectedChild) ?? null;
  });

  const fmt = (n: number): string => String(Math.round(n * 10) / 10);

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

<!--
  One layer row, rendered recursively for the whole tree. A group's own child
  objects and a composite's derived children nest under the same idiom: type
  glyph + id/label, an expand chevron when it has children, indented by depth.
  Selecting a group (or any of its descendants) selects the group — the unit
  the stage moves; drilling into a composite child selects that child.
-->
{#snippet layerRow(node: LayerNode, depth: number)}
  {@const open = isOpen(node.id)}
  <div class="obj-row" style:padding-left={`${depth * 12}px`}>
    <button
      class="obj"
      class:on={rowActive(node)}
      class:child={node.kind === ""}
      onclick={() => selectNode(node)}
    >
      {#if node.kind !== ""}<span class="obj-kind">{node.kind}</span>{/if}
      <span class="obj-id">{node.label}</span>
    </button>
    {#if node.children.length > 0}
      <button
        class="twist"
        aria-expanded={open}
        aria-label={`${open ? "collapse" : "expand"} ${node.label}`}
        onclick={() => (expanded[node.id] = !open)}
      >{open ? "▾" : "▸"}</button>
    {/if}
  </div>
  {#if node.children.length > 0 && open}
    {#each node.children as c (c.id)}
      {@render layerRow(c, depth + 1)}
    {/each}
  {/if}
{/snippet}

<aside class="rail">
  <div class="rail-title">{title}</div>
  <div class="outline">
    {#each tree as node (node.id)}
      {@render layerRow(node, 0)}
    {/each}
    {#if objects.length === 0}
      <div class="empty">no objects on this page</div>
    {/if}
  </div>

  {#if selectedChild !== null}
    <!-- A derived child: geometry is the layout's, not the file's, so the
         numbers are read-only — a node drag pins `nodes.<i>.at` instead. -->
    <div class="inspector">
      <div class="insp-head">{selectedChild}</div>
      {#if selectedChildFrame !== null}
        <div class="insp-unit mono">
          at [{fmt(selectedChildFrame.frame[0])}, {fmt(selectedChildFrame.frame[1])}] · size [{fmt(
            selectedChildFrame.frame[2],
          )}, {fmt(selectedChildFrame.frame[3])}]
        </div>
      {/if}
      <div class="insp-unit">layout-derived · drag a node to pin it</div>
    </div>
  {:else if selectedObj !== null && selectedObj.at !== null && selectedObj.size !== null}
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

  <!-- Board-level appearance. A board matches the app's light/dark by DEFAULT
       (no theme pinned, no ground override) — the picker states that as "Match
       app" rather than asking the user to choose. Everything below is optional:
       a scheme family (still follows the app's mode) or a fixed canvas ground. -->
  <div class="theme-sect">
    <div class="insp-sect">theme</div>
    {#if oncommittheme !== undefined && schemes.length > 0}
      <div class="scheme-row" role="group" aria-label="board theme">
        <button
          class="scheme"
          class:on={themeSelection === "auto"}
          title="match the app — follow its light/dark automatically (default)"
          aria-pressed={themeSelection === "auto"}
          onclick={() => oncommittheme?.(null)}>Match app</button
        >
        {#each schemes as s (s.id)}
          <button
            class="scheme"
            class:on={themeSelection === s.id}
            title={`${s.label} scheme — ${s.variant} in this mode`}
            aria-pressed={themeSelection === s.id}
            onclick={() => oncommittheme?.(s.id)}>{s.label}</button
          >
        {/each}
        {#if themeSelection === "pinned"}
          <span class="scheme pinned" title="a pinned theme — the app's mode no longer moves it"
            >pinned</span
          >
        {/if}
      </div>
      <div class="theme-note">{themeNote}</div>
    {/if}
    <!-- The canvas ground: "match theme" (the default) lets the ground follow
         the theme/app; white and black are literal grounds; the rest are THIS
         theme's own tones. The file's literal value marks the active swatch. -->
    <div class="insp-row swatch-row" role="group" aria-label="canvas ground">
      <span class="swatch-label">ground</span>
      <button
        class="swatch auto"
        class:on={canvasBackground === null}
        title="match theme"
        aria-label="canvas ground: match theme"
        onclick={() => oncommitcanvas(null)}
      >–</button>
      {#each GROUNDS as g (g.value)}
        <button
          class="swatch"
          class:on={activeGround === g.value}
          style:background={g.value}
          title={g.label}
          aria-label={`canvas ground ${g.label}`}
          onclick={() => oncommitcanvas(g.value)}
        ></button>
      {/each}
      {#each bgSwatches as s (s.token)}
        <button
          class="swatch"
          class:on={canvasBackground === s.token}
          style:background={s.hex}
          title={s.token}
          aria-label={`canvas ground ${s.token}`}
          onclick={() => oncommitcanvas(s.token)}
        ></button>
      {/each}
    </div>
  </div>

  {#if lintNotes.length > 0}
    <!-- Lint notes are collapsed by default (the user's ask): a quiet badge
         that expands to the list on click; nothing shows when there are none. -->
    <div class="lint-sect">
      <button
        class="lint-toggle"
        class:open={lintOpen}
        aria-expanded={lintOpen}
        onclick={() => (lintOpen = !lintOpen)}
      >
        <span class="lint-badge">⚠ {lintNotes.length}</span>
        <span class="lint-word">{lintNotes.length === 1 ? "note" : "notes"}</span>
        <span class="lint-tw">{lintOpen ? "▾" : "▸"}</span>
      </button>
      {#if lintOpen}
        <div class="lint-list">
          {#each lintNotes as w (w.rendered)}
            <div class="lint-item" class:err={w.severity === "error"}>{w.rendered}</div>
          {/each}
        </div>
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
  .obj-row {
    display: flex;
    align-items: center;
  }
  .obj-row .obj {
    flex: 1;
    min-width: 0;
  }
  .twist {
    flex-shrink: 0;
    width: 18px;
    padding: 2px 0;
    background: none;
    border: none;
    border-radius: 4px;
    color: var(--muted);
    font-size: var(--text-xs);
    line-height: 1;
    cursor: pointer;
  }
  .twist:hover {
    background: var(--row-hover);
    color: var(--fg);
  }
  /* Composite children carry no type glyph and read muted — indentation is
     the depth padding on .obj-row, not a fixed inset. */
  .obj.child .obj-id {
    color: var(--muted);
  }
  .obj.child.on .obj-id {
    color: var(--fg);
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
  .theme-sect {
    border-top: 1px solid var(--edge);
    padding: 0 12px 10px;
  }
  .theme-sect .insp-sect {
    margin-top: 8px;
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
  .insp-unit.mono {
    font-family: var(--mono);
    color: var(--fg);
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
  .scheme-row {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    margin-top: 4px;
  }
  .scheme {
    padding: 2px 8px;
    background: none;
    border: 1px solid var(--edge);
    border-radius: 999px;
    color: var(--fg);
    font-size: var(--text-xs);
    cursor: pointer;
  }
  .scheme:hover {
    background: var(--row-hover);
    border-color: var(--accent);
  }
  .scheme.on {
    background: color-mix(in srgb, var(--accent) 14%, transparent);
    border-color: var(--accent);
    color: var(--accent);
  }
  .scheme.pinned {
    /* A state, not a button — the app's mode no longer moves the board. */
    color: var(--muted);
    border-style: dashed;
    cursor: default;
  }
  .theme-note {
    margin-top: 4px;
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .lint-sect {
    border-top: 1px solid var(--edge);
    padding: 4px 8px 6px;
  }
  .lint-toggle {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    padding: 3px 4px;
    background: none;
    border: none;
    border-radius: 4px;
    color: var(--warn);
    font-size: var(--text-xs);
    cursor: pointer;
  }
  .lint-toggle:hover {
    background: var(--row-hover);
  }
  .lint-badge {
    font-family: var(--mono);
    flex-shrink: 0;
  }
  .lint-word {
    color: var(--muted);
  }
  .lint-tw {
    margin-left: auto;
    color: var(--muted);
    line-height: 1;
  }
  .lint-list {
    padding: 2px 4px 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
    max-height: 132px;
    overflow-y: auto;
    scrollbar-width: thin;
  }
  .lint-item {
    font-size: var(--text-xs);
    font-family: var(--mono);
    color: var(--warn);
    overflow-wrap: anywhere;
  }
  .lint-item.err {
    color: var(--err);
  }
</style>
