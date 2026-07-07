<script lang="ts">
  /**
   * The agent launcher popover (DESIGN.md "The agent launcher"), opened by
   * the split button's chevron ONLY — hover (~150ms) on the chevron or a
   * click; the main surface is a pure instant spawn and never opens this.
   *
   * Two questions, nothing else:
   * - WHICH agent: one row per known CLI; installed agents are selectable,
   *   uninstalled ones stay visible but muted with an "install" action that
   *   pre-types the install command into a fresh terminal (never executes);
   * - NEW or RESUMED: a "resume" section listing the workspace's resumable
   *   Claude conversations (title + relative age), searchable past 8.
   *
   * Every selection becomes the new persisted default and spawns
   * immediately into the focused pane (the parent owns both effects).
   *
   * Keyboard: one highlight across ALL rows (agents + resume); ArrowUp/Down
   * moves it, Enter activates, Escape closes. Typing filters the resume list.
   */
  import { onMount } from "svelte";
  import {
    getAgentDefault,
    listAgents,
    listResumables,
    relativeAge,
    type AgentInfo,
    type LaunchPick,
    type ResumeEntry,
  } from "./launcher";
  import SessionGlyph from "./SessionGlyph.svelte";

  interface Props {
    workspaceId: string;
    /** The split button's rect; the popover hangs below it (fixed). */
    anchor: DOMRect;
    onPick: (pick: LaunchPick) => void;
    onInstall: (agent: AgentInfo) => void;
    onClose: () => void;
  }

  let { workspaceId, anchor, onPick, onInstall, onClose }: Props = $props();

  let agents = $state<AgentInfo[] | null>(null);
  let resumables = $state<ResumeEntry[]>([]);
  let loadError = $state<string | null>(null);
  /** Loading is invisible <150ms, then a soft pulse (polish inventory). */
  let showLoading = $state(false);

  /** The one highlight, across agent rows and resume rows. */
  let hl = $state(0);
  let query = $state("");
  let rootEl = $state<HTMLElement | null>(null);
  let listEl = $state<HTMLElement | null>(null);

  const SEARCH_AT = 8;

  const filteredResume = $derived.by(() => {
    const q = query.trim().toLowerCase();
    if (q === "") return resumables;
    return resumables.filter((r) => r.title.toLowerCase().includes(q));
  });

  /** Flat keyboard order: one item per agent row, then the resume rows. */
  const itemCount = $derived((agents?.length ?? 0) + filteredResume.length);
  const agentCount = $derived(agents?.length ?? 0);

  $effect(() => {
    // Filtering can strand the highlight past the end; clamp, never lose it.
    if (hl >= itemCount) hl = Math.max(0, itemCount - 1);
  });

  onMount(() => {
    const def = getAgentDefault();
    const loadTimer = setTimeout(() => (showLoading = true), 150);
    void Promise.all([
      listAgents(),
      listResumables(workspaceId).catch(() => [] as ResumeEntry[]),
    ])
      .then(([a, r]) => {
        agents = a;
        resumables = r;
        hl = Math.max(
          0,
          a.findIndex((x) => x.id === def.agent),
        );
      })
      .catch((e) => {
        loadError = e instanceof Error ? e.message : "failed to load agents";
      })
      .finally(() => clearTimeout(loadTimer));

    rootEl?.focus();

    // Overlay language: any outside press or Escape closes. Presses on the
    // split button are the anchor's own toggle/spawn semantics — not
    // "outside" (closing here would race the button's click handler).
    const onDown = (e: PointerEvent) => {
      if (rootEl === null || !(e.target instanceof Node)) return;
      if (rootEl.contains(e.target)) return;
      if (e.target instanceof Element && e.target.closest(".new-split") !== null) return;
      onClose();
    };
    window.addEventListener("pointerdown", onDown, true);
    return () => window.removeEventListener("pointerdown", onDown, true);
  });

  /** Fixed position: below the anchor, clamped into the viewport.
   *  (clientWidth/Height fallbacks: embedded webviews can report
   *  window.inner* as 0.) */
  const pos = $derived.by(() => {
    const viewW = window.innerWidth || document.documentElement.clientWidth || 1280;
    const viewH = window.innerHeight || document.documentElement.clientHeight || 800;
    const width = 308;
    const left = Math.max(8, Math.min(anchor.left, viewW - width - 8));
    const top = anchor.bottom + 6;
    const maxH = Math.max(180, viewH - top - 12);
    return { left, top, width, maxH };
  });

  function pickAgent(a: AgentInfo): void {
    if (!a.installed) {
      onInstall(a);
      return;
    }
    onPick({ agent: a.id });
  }

  function pickResume(r: ResumeEntry): void {
    onPick({ agent: "claude", resume: r.id });
  }

  function activate(i: number): void {
    if (agents === null) return;
    if (i < agentCount) {
      pickAgent(agents[i]);
    } else {
      const r = filteredResume[i - agentCount];
      if (r !== undefined) pickResume(r);
    }
  }

  function move(delta: number): void {
    if (itemCount === 0) return;
    hl = (hl + delta + itemCount) % itemCount;
    // Keep the highlighted resume row in view (the list scrolls).
    const el = listEl?.querySelector(`[data-item="${hl}"]`);
    el?.scrollIntoView({ block: "nearest" });
  }

  function onKeydown(e: KeyboardEvent): void {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      onClose();
      return;
    }
    if (e.key === "ArrowDown") {
      e.preventDefault();
      move(1);
      return;
    }
    if (e.key === "ArrowUp") {
      e.preventDefault();
      move(-1);
      return;
    }
    if (e.key === "Enter") {
      e.preventDefault();
      activate(hl);
      return;
    }
    // Type-to-search the resume list from anywhere in the popover.
    if (resumables.length > SEARCH_AT) {
      if (e.key === "Backspace" && !e.metaKey && !e.ctrlKey && !e.altKey) {
        e.preventDefault();
        query = query.slice(0, -1);
      } else if (e.key.length === 1 && !e.metaKey && !e.ctrlKey && !e.altKey) {
        e.preventDefault();
        query += e.key;
      }
    }
  }
</script>

<div
  class="launcher"
  role="menu"
  aria-label="new agent"
  tabindex="-1"
  bind:this={rootEl}
  style:left="{pos.left}px"
  style:top="{pos.top}px"
  style:width="{pos.width}px"
  style:max-height="{pos.maxH}px"
  onkeydown={onKeydown}
>
  {#if agents === null}
    {#if loadError !== null}
      <div class="state err">{loadError}</div>
    {:else if showLoading}
      <div class="state pulse">checking installed agents…</div>
    {/if}
  {:else}
    <div class="agents">
      {#each agents as a, i (a.id)}
        <button
          class="arow"
          class:hl={hl === i}
          class:missing={!a.installed}
          role="menuitem"
          tabindex="-1"
          data-item={i}
          onpointerenter={() => (hl = i)}
          onclick={() => activate(i)}
        >
          <span class="gslot"><SessionGlyph kind="agent" agentKind={a.id} size={13} /></span>
          <span class="aname">{a.name}</span>
          {#if a.installed}
            {#if a.version !== null}
              <span class="aver" title={a.version}>{a.version.split(" ")[0]}</span>
            {/if}
          {:else}
            <span
              class="ainstall"
              title="pre-types “{a.install}” in a new terminal — you press Enter"
            >
              install
            </span>
          {/if}
        </button>
      {/each}
    </div>

    {#if resumables.length > 0}
      <div class="sec">resume</div>
      {#if resumables.length > SEARCH_AT}
        <input
          class="search"
          type="text"
          placeholder="search conversations…"
          bind:value={query}
          onkeydown={(e) => {
            // The root handler owns arrows/Enter/Escape; let text keys type.
            if (["ArrowDown", "ArrowUp", "Enter", "Escape"].includes(e.key)) return;
            e.stopPropagation();
          }}
        />
      {/if}
      <div class="rlist" bind:this={listEl}>
        {#each filteredResume as r, j (r.id)}
          {@const i = agentCount + j}
          <button
            class="rrow"
            class:hl={hl === i}
            role="menuitem"
            tabindex="-1"
            data-item={i}
            title={r.title}
            onpointerenter={() => (hl = i)}
            onclick={() => pickResume(r)}
          >
            <span class="rtitle">{r.title}</span>
            <span class="rage">{relativeAge(r.mtime)}</span>
          </button>
        {/each}
        {#if filteredResume.length === 0}
          <div class="state">no conversations match “{query}”</div>
        {/if}
      </div>
    {/if}

    <div class="foot">
      <span><kbd>↑↓</kbd> choose</span>
      <span><kbd>↵</kbd> start</span>
      <span><kbd>esc</kbd> close</span>
    </div>
  {/if}
</div>

<style>
  .launcher {
    position: fixed;
    z-index: 120;
    display: flex;
    flex-direction: column;
    padding: 5px;
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 10px;
    box-shadow:
      0 1px 2px rgba(0, 0, 0, 0.08),
      0 12px 32px rgba(0, 0, 0, 0.22);
    outline: none;
    animation: pop 0.12s ease-out;
  }

  @keyframes pop {
    from {
      opacity: 0;
      transform: translateY(-3px) scale(0.985);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .launcher {
      animation: none;
    }
  }

  .state {
    padding: 8px 10px;
    font-size: var(--text-sm);
    color: var(--muted);
  }

  .state.err {
    color: var(--err);
  }

  .state.pulse {
    animation: soft 1.2s ease-in-out infinite;
  }

  @keyframes soft {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0.45;
    }
  }

  .agents {
    flex: none;
  }

  .arow {
    appearance: none;
    border: none;
    background: none;
    width: 100%;
    text-align: left;
    font: inherit;
    color: var(--fg);
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 7px 9px;
    border-radius: 6px;
    font-size: var(--text-md);
    cursor: pointer;
    user-select: none;
  }

  .arow.hl {
    background: var(--row-hover);
  }

  /* Fixed glyph slot so agent names align regardless of mark width. */
  .gslot {
    flex: none;
    width: 16px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
  }

  .aname {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 500;
  }

  .aver {
    flex: none;
    max-width: 96px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }

  /* Uninstalled agents: visible but muted — honest, not hidden. */
  .arow.missing .aname {
    color: var(--muted);
    font-weight: 400;
  }

  .arow.missing :global(.sglyph) {
    opacity: 0.55;
  }

  .ainstall {
    flex: none;
    padding: 1px 8px;
    border: 1px solid var(--edge);
    border-radius: 10px;
    font-size: var(--text-xs);
    color: var(--muted);
    transition:
      color 0.12s ease,
      border-color 0.12s ease;
  }

  .arow.missing.hl .ainstall,
  .arow.missing:hover .ainstall {
    color: var(--fg);
    border-color: color-mix(in srgb, var(--fg) 30%, transparent);
  }

  .sec {
    padding: 9px 9px 4px;
    border-top: 1px solid var(--edge);
    margin-top: 5px;
    font-size: var(--text-xs);
    font-weight: 600;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--muted);
    user-select: none;
  }

  .search {
    margin: 0 5px 4px;
    padding: 3px 8px;
    border: 1px solid var(--edge);
    border-radius: 6px;
    background: none;
    font: inherit;
    font-size: var(--text-sm);
    color: var(--fg);
    outline: none;
  }

  .search:focus {
    border-color: color-mix(in srgb, var(--accent) 55%, transparent);
  }

  .search::placeholder {
    color: var(--muted);
  }

  .rlist {
    /* Intrinsic height (basis auto), shrink under a tight max-height but
       never below ~3 rows — `flex: 1` (basis 0) collapsed the whole list
       to 0px whenever the popover's max-height clamped. */
    flex: 0 1 auto;
    min-height: 84px;
    overflow-y: auto;
    scrollbar-width: thin;
    /* Soft edge fade when the list overflows (polish inventory). */
    mask-image: linear-gradient(to bottom, black calc(100% - 10px), transparent);
  }

  .rrow {
    appearance: none;
    border: none;
    background: none;
    width: 100%;
    text-align: left;
    font: inherit;
    color: var(--fg);
    display: flex;
    align-items: baseline;
    gap: 8px;
    padding: 5px 9px 5px 34px;
    border-radius: 6px;
    cursor: pointer;
    user-select: none;
  }

  .rrow.hl {
    background: var(--row-hover);
  }

  .rtitle {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
  }

  .rage {
    flex: none;
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    color: var(--muted);
  }

  .foot {
    flex: none;
    display: flex;
    align-items: center;
    gap: 12px;
    margin-top: 5px;
    padding: 6px 9px 2px;
    border-top: 1px solid var(--edge);
    font-size: var(--text-xs);
    color: var(--muted);
    user-select: none;
  }

  .foot kbd {
    font-family: var(--mono);
    font-size: 0.66rem;
    padding: 0 0.25rem;
    border: 1px solid var(--edge);
    border-radius: 4px;
  }
</style>
