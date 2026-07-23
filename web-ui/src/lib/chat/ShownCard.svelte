<script lang="ts">
  import { basename, fsBoardRender, fsFile, fsList, joinPath } from "../previews/files";
  import { ApiError } from "../net/api";
  import { fsCopyOp } from "../workspace/fsEvents";
  import { fsMkdir } from "../workspace/sessions";
  import Spinner from "../previews/Spinner.svelte";
  import {
    boardsDirFor,
    chartProvenance,
    hasProvenanceDetail,
    uniqueBoardName,
    workspaceRootFor,
    type ChartProvenance,
  } from "./shownBoards";

  /**
   * The agent "showing you something" mid-work via `chimaera board show`
   * (docs/board-plan.md §10.1) — a first-class figure in the conversation
   * flow. ToolGroup mounts the full card in the transcript (visible while
   * the command group stays collapsed); ToolCallCard mounts the `compact`
   * reference row under the producing command. Detection is client-side v1
   * (shownBoards.ts) — the planned daemon-injected `shown` journal event can
   * replace it without touching this card. The render is server-side and
   * content-addressed, so a re-mount is a cache hit; a same-`--id` re-show
   * bumps `revision` and the mounted card refetches in place.
   *
   * Actions (full mode, hover-revealed): a "data" provenance disclosure
   * (origin + bound source/inputs + trace, parsed from the board file the
   * card already renders), "save to boards/" — the explicit promotion that
   * copies the throwaway into `<workspace>/boards/` — and open-in-pane.
   */
  interface Props {
    /** The .board.json path (already resolved by the caller). */
    path: string;
    /** Open a path in a file tab (the workbench path-click flow). */
    onOpen?: (path: string) => void;
    /** Bumped by shownBoards' reduction on a same-path re-show. */
    revision?: number;
    /** Head-only reference row (no image, no actions) for tool rows. */
    compact?: boolean;
    /** Session working directory — anchors boards/ and provenance paths. */
    cwd?: string;
    /** False while the owning retained chat tab is hidden. */
    visible?: boolean;
  }

  let { path, onOpen, revision = 1, compact = false, cwd, visible = true }: Props = $props();

  let imgUrl = $state<string | null>(null);
  let size = $state<[number, number] | null>(null);
  let error = $state<string | null>(null);
  let provenance = $state<ChartProvenance | null>(null);
  let dataOpen = $state(false);

  $effect(() => {
    const p = path;
    void revision; // a re-show re-renders the same path in place
    if (compact) return;
    let cancelled = false;
    error = null;
    // Keep the previous image up while a re-show refetches — swapping in
    // place must not flash a spinner over an already-presented figure.
    fsBoardRender(p, 0).then(
      (r) => {
        if (cancelled) return;
        imgUrl = `/raw/${r.ticket}`;
        size = [r.width, r.height];
      },
      (err: unknown) => {
        if (cancelled) return;
        if (imgUrl === null) error = err instanceof Error ? err.message : String(err);
      },
    );
    return () => {
      cancelled = true;
    };
  });

  // The provenance disclosure reads the same tiny .board.json the card
  // renders. Anything malformed/truncated is simply "no disclosure".
  $effect(() => {
    const p = path;
    void revision;
    if (compact) return;
    let cancelled = false;
    fsFile(p).then(
      (chunk) => {
        if (cancelled || chunk.truncated) return;
        provenance = chartProvenance(new TextDecoder().decode(chunk.bytes));
      },
      () => {
        if (!cancelled) provenance = null;
      },
    );
    return () => {
      cancelled = true;
    };
  });

  const detail = $derived(hasProvenanceDetail(provenance));
  const boardsDir = $derived(boardsDirFor(path, cwd));
  const wsRoot = $derived(workspaceRootFor(path, cwd));

  /** Resolve a workspace-relative provenance path for open-in-pane. */
  const resolveInput = (rel: string): string | null =>
    rel.startsWith("/") ? rel : wsRoot !== null ? joinPath(wsRoot, rel.replace(/^\.\//, "")) : null;

  // --- save to boards/: the confirmed promotion --------------------------
  let saving = $state(false);
  let savedPath = $state<string | null>(null);
  let saveError = $state<string | null>(null);

  async function saveToBoards() {
    const dir = boardsDir;
    if (dir === null || saving || savedPath !== null) return;
    saving = true;
    saveError = null;
    try {
      const canonical = await fsMkdir(dir);
      // Client-side unique naming keeps the compound .board.json extension
      // whole; two attempts cover a create race (409 → re-pick once).
      for (let attempt = 0; ; attempt++) {
        const listing = await fsList(canonical, true);
        const existing = new Set(listing.entries.map((e) => e.name));
        const name = uniqueBoardName(basename(path), existing);
        try {
          savedPath = await fsCopyOp(path, joinPath(canonical, name), "fail");
          break;
        } catch (err) {
          if (attempt === 0 && err instanceof ApiError && err.status === 409) continue;
          throw err;
        }
      }
    } catch (err) {
      saveError = err instanceof Error ? err.message : String(err);
    } finally {
      saving = false;
    }
  }
</script>

<div class="shown" class:compact class:visible>
  <div class="head">
    <button class="name-btn" title="open {basename(path)} in a pane" onclick={() => onOpen?.(path)}>
      <span class="chip">board</span>
      <span class="name">{basename(path)}</span>
    </button>
    {#if !compact}
      <span class="actions">
        {#if detail}
          <button
            class="act text"
            class:on={dataOpen}
            aria-pressed={dataOpen}
            title="where the numbers came from"
            onclick={() => (dataOpen = !dataOpen)}>data</button
          >
        {/if}
        {#if boardsDir !== null && savedPath === null}
          <button
            class="act text"
            disabled={saving}
            title="keep this board — copy it into {boardsDir}"
            onclick={saveToBoards}>{saving ? "saving…" : "save to boards/"}</button
          >
        {/if}
        <button class="act" title="open in a pane" onclick={() => onOpen?.(path)}>
          <svg viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
            <path
              d="M6 4h6v6M12 4l-7 7"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
              stroke-linejoin="round"
            />
          </svg>
        </button>
      </span>
    {/if}
  </div>
  {#if !compact}
    {#if error !== null}
      <!-- Quiet failure: an expired/unrenderable board never shows a broken img. -->
      <div class="err">board preview unavailable — {error}</div>
    {:else if imgUrl !== null}
      <button class="stage" title="open in a pane" onclick={() => onOpen?.(path)}>
        <img
          src={imgUrl}
          alt={basename(path)}
          width={size?.[0]}
          height={size?.[1]}
          loading="lazy"
          decoding="async"
        />
      </button>
    {:else}
      <div class="loading">
        <Spinner />
      </div>
    {/if}
    {#if dataOpen && provenance !== null}
      <div class="data">
        {#if provenance.origin !== null}
          <div class="row"><span class="k">origin</span><span class="v">{provenance.origin}</span></div>
        {/if}
        {#if provenance.source !== null}
          {@const resolved = resolveInput(provenance.source)}
          <div class="row">
            <span class="k">source</span>
            {#if resolved !== null && onOpen !== undefined}
              <button class="path" title="open {resolved} in a pane" onclick={() => onOpen?.(resolved)}
                >{provenance.source}</button
              >
            {:else}
              <span class="v mono">{provenance.source}</span>
            {/if}
          </div>
        {/if}
        {#if provenance.inputs.length > 0}
          <div class="row">
            <span class="k">inputs</span>
            <span class="v inputs">
              {#each provenance.inputs as inp (inp)}
                {@const resolved = resolveInput(inp)}
                {#if resolved !== null && onOpen !== undefined}
                  <button class="path" title="open {resolved} in a pane" onclick={() => onOpen?.(resolved)}
                    >{inp}</button
                  >
                {:else}
                  <span class="v mono">{inp}</span>
                {/if}
              {/each}
            </span>
          </div>
        {/if}
        {#if provenance.trace !== null}
          <div class="trace">{provenance.trace}</div>
        {/if}
      </div>
    {/if}
    {#if savedPath !== null}
      <div class="saved">
        <span class="saved-text">saved → boards/{basename(savedPath)}</span>
        {#if onOpen !== undefined}
          {@const kept = savedPath}
          <button class="act text" title="open the saved board in a pane" onclick={() => onOpen?.(kept)}
            >open</button
          >
        {/if}
      </div>
    {:else if saveError !== null}
      <div class="saved err-line" title={saveError}>save failed — {saveError}</div>
    {/if}
  {/if}
</div>

<style>
  /* Full mode is a first-class figure in the transcript column — the agent
     presenting, not CLI residue — so it wears the ToolGroup container idiom
     (edge border, rise entrance) at reading width. */
  .shown {
    margin: 4px 0;
    border: 1px solid var(--edge);
    border-radius: 8px;
    overflow: hidden;
    background: color-mix(in srgb, var(--fg) 2%, transparent);
    animation: rise 0.15s ease; /* @keyframes rise lives in app.css */
  }
  .shown:not(.visible) {
    animation: none;
  }
  @media (prefers-reduced-motion: reduce) {
    .shown {
      animation: none;
    }
  }
  /* Compact: the small per-command reference row inside a tool group. */
  .shown.compact {
    margin: 6px 10px 8px;
    animation: none;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    padding: 0 8px 0 0;
  }
  .name-btn {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 6px;
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
  .name-btn:hover {
    background: color-mix(in srgb, var(--fg) 4%, transparent);
  }
  .chip {
    flex: none;
    padding: 0 6px;
    border-radius: 999px;
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    font-family: var(--mono, monospace);
    font-size: var(--text-xs);
  }
  .name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono, monospace);
  }
  /* Hover-revealed actions (the tool-card copy-button language); keyboard
     focus and an open data panel keep them visible. */
  .actions {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 8px;
    opacity: 0;
    transition: opacity 0.12s ease;
  }
  .shown:hover .actions,
  .actions:focus-within,
  .actions:has(.on) {
    opacity: 1;
  }
  .act {
    flex: none;
    display: inline-flex;
    align-items: center;
    background: none;
    border: none;
    color: var(--muted);
    padding: 2px;
    cursor: pointer;
    transition: color 0.12s ease;
  }
  .act:hover:not(:disabled) {
    color: var(--accent);
  }
  .act:disabled {
    cursor: default;
  }
  .act.text {
    font-family: var(--mono, monospace);
    font-size: var(--text-xs);
    white-space: nowrap;
  }
  .act.text.on {
    color: var(--accent);
  }
  .stage {
    display: block;
    width: 100%;
    padding: 10px;
    background: none;
    border: none;
    border-top: 1px solid color-mix(in srgb, var(--edge) 55%, transparent);
    cursor: zoom-in;
    text-align: center;
  }
  /* Sized like a real figure: the shown preset is 720×450 @2×, so it fills
     the ~600–700px transcript measure edge to edge. */
  .stage img {
    width: 100%;
    height: auto;
    max-height: 460px;
    object-fit: contain;
    border-radius: 4px;
  }
  .loading {
    position: relative;
    min-height: 160px;
    border-top: 1px solid color-mix(in srgb, var(--edge) 55%, transparent);
  }
  .err {
    padding: 5px 10px;
    border-top: 1px solid color-mix(in srgb, var(--edge) 55%, transparent);
    color: var(--muted);
    font-size: var(--text-sm);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* The provenance disclosure: quiet key/value rows + the trace as prose. */
  .data {
    padding: 6px 10px 8px;
    border-top: 1px solid color-mix(in srgb, var(--edge) 55%, transparent);
    display: flex;
    flex-direction: column;
    gap: 3px;
    font-size: var(--text-sm);
  }
  .row {
    display: flex;
    align-items: baseline;
    gap: 8px;
    min-width: 0;
  }
  .k {
    flex: none;
    width: 44px;
    color: var(--muted);
    font-family: var(--mono, monospace);
    font-size: var(--text-xs);
  }
  .v {
    min-width: 0;
    color: var(--fg);
  }
  .mono {
    font-family: var(--mono, monospace);
  }
  .inputs {
    display: flex;
    flex-wrap: wrap;
    gap: 2px 10px;
  }
  .path {
    background: none;
    border: none;
    padding: 0;
    color: var(--accent);
    font-family: var(--mono, monospace);
    font-size: var(--text-sm);
    cursor: pointer;
    text-align: left;
    overflow-wrap: anywhere;
  }
  .path:hover {
    text-decoration: underline;
  }
  .trace {
    color: var(--muted);
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 120px;
    overflow: auto;
    scrollbar-width: thin;
  }
  /* The promotion receipt: where the board now lives, and a way there. */
  .saved {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 5px 10px;
    border-top: 1px solid color-mix(in srgb, var(--edge) 55%, transparent);
    font-size: var(--text-sm);
  }
  .saved-text {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: color-mix(in srgb, var(--accent) 85%, var(--fg));
    font-family: var(--mono, monospace);
    font-size: var(--text-xs);
  }
  .saved .act {
    opacity: 1;
  }
  .err-line {
    color: var(--err);
    font-size: var(--text-xs);
    font-family: var(--mono, monospace);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
