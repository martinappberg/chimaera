<script lang="ts">
  /**
   * The pane's always-present top bar (~26px): type glyph + tab name per
   * tab (active emphasized by WEIGHT, not color), pane controls at the
   * right edge (fade in on bar hover; the zoom badge stays persistent
   * while zoomed). The bar's empty area is a drag handle for the active
   * tab, so every pane can always be re-tiled by its bar.
   *
   * Surface parity: terminal and file tabs share one anatomy — glyph +
   * name + close, same drag, same middle-click close, same dblclick zoom.
   * A terminal's glyph carries its session-state color.
   */
  import { tabKey, type PaneNode, type Tab } from "./layout";
  import type { Session } from "./sessions";
  import { dotState, dotTitle } from "./sessions";
  import type { DropSpot, LayoutCtrl } from "./dnd";
  import { agentHue, type LinkCtrl } from "./links";
  import { basename } from "./files";
  import { dirtyFiles } from "./editing";
  import { KEYS } from "./keys";
  import FileIcon from "./FileIcon.svelte";

  interface Props {
    node: PaneNode;
    /** True while this pane is rendered zoomed (fullscreen in the window). */
    zoomed?: boolean;
    sessions: Map<string, Session>;
    names: Map<string, string>;
    /** Open-file tab titles (basename, disambiguated), keyed by path. */
    fileNames: Map<string, string>;
    /** terminal session id -> agent session id (the linked-terminal edges). */
    links: Map<string, string>;
    linkCtrl: LinkCtrl;
    dropSpot: DropSpot | null;
    ctrl: LayoutCtrl;
    /** Bound by Pane so the dnd hit-tester can target this bar. */
    el?: HTMLElement | null;
  }

  let {
    node,
    zoomed = false,
    sessions,
    names,
    fileNames,
    links,
    linkCtrl,
    dropSpot,
    ctrl,
    el = $bindable(null),
  }: Props = $props();

  const activeSession = $derived.by(() => {
    const tab = node.tabs[node.active];
    return tab !== undefined && tab.surface === "terminal"
      ? (sessions.get(tab.sessionId) ?? null)
      : null;
  });

  /** Chips on an AGENT pane: the terminals this agent holds a leash to. */
  const linkedTerminals = $derived.by(() => {
    if (activeSession === null || activeSession.kind !== "agent") return [];
    const out: string[] = [];
    for (const [terminal, agent] of links) {
      if (agent === activeSession.id) out.push(terminal);
    }
    return out;
  });

  /** Back-reference on a TERMINAL pane: the agent holding its leash. */
  const linkedAgentId = $derived(
    activeSession !== null && activeSession.kind === "shell"
      ? (links.get(activeSession.id) ?? null)
      : null,
  );

  function sessionLabel(id: string): string {
    return names.get(id) ?? sessions.get(id)?.name ?? id.slice(0, 8);
  }

  /** Chip dot modifier for a linked terminal: what is that shell doing? */
  function chipState(id: string): string {
    const s = sessions.get(id);
    if (s === undefined || !s.alive) return "quiet";
    if (s.exec_stage === "executing") return "exec";
    if (s.exec_stage === "queued") return "queued";
    if (s.phase === "running") return "busy";
    return "ready";
  }

  function chipTitle(id: string): string {
    const s = sessions.get(id);
    const doing =
      s === undefined || !s.alive
        ? "exited"
        : s.exec_stage === "executing"
          ? "agent is running a command here"
          : s.exec_stage === "queued"
            ? "agent exec queued for the prompt"
            : s.phase === "running"
              ? "a command is running"
              : "at the prompt";
    return `linked terminal · ${doing} — click to focus`;
  }

  // --- "link to agent…" menu (parity path for the drag gesture) ------------

  let linkMenuOpen = $state(false);
  let barRightEl = $state<HTMLElement | null>(null);

  /** Live agents in this terminal's workspace, offered by the link menu. */
  const agentChoices = $derived.by(() => {
    if (activeSession === null || activeSession.kind !== "shell") return [];
    return [...sessions.values()].filter(
      (s) =>
        s.kind === "agent" && s.alive && s.workspace_id === activeSession.workspace_id,
    );
  });

  // Close the menu on any press outside the bar's right cluster or Escape.
  $effect(() => {
    if (!linkMenuOpen) return;
    const onDown = (e: PointerEvent) => {
      if (!(e.target instanceof Node) || barRightEl?.contains(e.target) !== true) {
        linkMenuOpen = false;
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        linkMenuOpen = false;
      }
    };
    window.addEventListener("pointerdown", onDown, true);
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("pointerdown", onDown, true);
      window.removeEventListener("keydown", onKey, true);
    };
  });

  function chooseAgent(agentId: string): void {
    if (activeSession === null) return;
    if (linkedAgentId === agentId) {
      linkCtrl.unlink(activeSession.id);
    } else {
      linkCtrl.link(activeSession.id, agentId);
    }
    linkMenuOpen = false;
  }

  /** Insertion index while a drag hovers this tab bar, else null. */
  const insertIndex = $derived(
    dropSpot?.kind === "tab" && dropSpot.paneId === node.id ? dropSpot.index : null,
  );

  function label(tab: Tab): string {
    if (tab.surface === "terminal") {
      return names.get(tab.sessionId) ?? sessions.get(tab.sessionId)?.name ?? tab.sessionId.slice(0, 8);
    }
    return fileNames.get(tab.path) ?? basename(tab.path);
  }

  /** Empty bar area drags the pane's ACTIVE tab (capture runs before the
   *  tabs' own handlers; anything inside a tab or a button is theirs). */
  function onBarPointerDown(e: PointerEvent): void {
    if (!(e.target instanceof Element)) return;
    if (e.target.closest("[data-tab-index], button") !== null) return;
    const active = node.tabs[node.active];
    if (active !== undefined) ctrl.dragTab(e, node.id, node.active, active);
  }
</script>

<div class="bar" bind:this={el} onpointerdowncapture={onBarPointerDown}>
  <div class="tabs" role="tablist">
    {#each node.tabs as tab, i (tabKey(tab))}
      <div
        class="tab"
        class:active={i === node.active}
        class:insert={insertIndex === i}
        role="tab"
        aria-selected={i === node.active}
        tabindex="-1"
        data-tab-index={i}
        title={tab.surface === "file" ? tab.path : label(tab)}
        onpointerdowncapture={(e) => {
          // Capture-phase (directly attached, not delegated); ignore presses
          // on the close button so it stays a plain click.
          if (e.target instanceof Element && e.target.closest(".tab-close")) return;
          ctrl.dragTab(e, node.id, i, tab);
        }}
        onauxclick={(e) => {
          // Middle-click closes the tab (detaches the view, never the session).
          if (e.button === 1) {
            e.preventDefault();
            ctrl.closeTab(node.id, i);
          }
        }}
        ondblclick={() => ctrl.zoomPane(node.id)}
      >
        {#if tab.surface === "terminal"}
          {@const s = sessions.get(tab.sessionId)}
          <svg class="glyph {s ? dotState(s) : ''}" viewBox="0 0 16 16" width="10" height="10" aria-hidden="true">
            <title>{s ? dotTitle(s) : "terminal"}</title>
            <path
              d="M3 4.5L6.5 8 3 11.5M8.5 12h4.5"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
              stroke-linejoin="round"
            />
          </svg>
        {:else if $dirtyFiles.has(tab.path)}
          <!-- Dirty dot replaces the type glyph in its slot (unsaved edits). -->
          <span class="dirty-dot" title="unsaved changes"></span>
        {:else}
          <span class="tab-glyph" class:on={i === node.active}>
            <FileIcon path={tab.path} size={13} plain={i === node.active} />
          </span>
        {/if}
        <span class="tab-name">{label(tab)}</span>
        <button
          class="tab-close"
          aria-label="close tab"
          title="close tab"
          onclick={(e) => {
            e.stopPropagation();
            ctrl.closeTab(node.id, i);
          }}>&times;</button
        >
      </div>
    {/each}
    <div class="tab-tail" class:insert={insertIndex === node.tabs.length}></div>
  </div>

  <!-- Linked-terminal chips: on an agent pane, the complete map of the
       terminals it holds; on a linked terminal, the way back to its agent.
       Always visible (the bond is state, not chrome), hue = the agent's. -->
  {#if linkedTerminals.length > 0 && activeSession !== null}
    {@const hue = agentHue(activeSession.id)}
    <div class="links" role="group" aria-label="linked terminals">
      {#each linkedTerminals as tid (tid)}
        <span class="chip" style:--hue={hue}>
          <button class="chip-main" title={chipTitle(tid)} onclick={() => linkCtrl.reveal(tid, node.id)}>
            <span class="chip-dot {chipState(tid)}"></span>
            <span class="chip-name">{sessionLabel(tid)}</span>
          </button>
          <button class="chip-x" title="unlink" aria-label="unlink {sessionLabel(tid)}" onclick={() => linkCtrl.unlink(tid)}>&times;</button>
        </span>
      {/each}
    </div>
  {:else if linkedAgentId !== null && activeSession !== null}
    <div class="links" role="group" aria-label="linked agent">
      <span class="chip" style:--hue={agentHue(linkedAgentId)}>
        <button
          class="chip-main"
          title="linked to this agent — click to jump"
          onclick={() => linkCtrl.reveal(linkedAgentId, node.id)}
        >
          <svg class="chip-spark" viewBox="0 0 16 16" width="9" height="9" aria-hidden="true">
            <path d="M8 1.5v13M1.5 8h13M3.9 3.9l8.2 8.2M12.1 3.9l-8.2 8.2" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" />
          </svg>
          <span class="chip-name">{sessionLabel(linkedAgentId)}</span>
        </button>
        <button class="chip-x" title="unlink" aria-label="unlink from agent" onclick={() => linkCtrl.unlink(activeSession.id)}>&times;</button>
      </span>
    </div>
  {/if}

  <!-- Pane controls at the bar's right edge: the mouse path to every pane
       chord (tooltips teach the chords). Faded in on bar hover; the zoom
       badge stays persistent while zoomed. -->
  <div class="bar-right" bind:this={barRightEl}>
    {#if zoomed}
      <button class="zoom-badge" title="exit zoom ({KEYS.zoom})" onclick={() => ctrl.zoomPane(node.id)}
        >zoom</button
      >
    {/if}
    <div class="controls">
      {#if agentChoices.length > 0}
        <button
          class="ctl"
          class:on={linkMenuOpen}
          title={linkedAgentId !== null ? "linked — move or unlink" : "link to agent…"}
          aria-label="link to agent"
          aria-expanded={linkMenuOpen}
          onclick={() => (linkMenuOpen = !linkMenuOpen)}
        >
          <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
            <path
              d="M6.5 9.5l3-3M5 7l-1.8 1.8a2.3 2.3 0 003.2 3.2L8.2 10.2M11 9l1.8-1.8a2.3 2.3 0 00-3.2-3.2L7.8 5.8"
              fill="none"
              stroke="currentColor"
              stroke-width="1.3"
              stroke-linecap="round"
            />
          </svg>
        </button>
      {/if}
      <button
        class="ctl"
        title="split right ({KEYS.splitRight})"
        aria-label="split right"
        onclick={() => ctrl.splitPaneAt(node.id, "row")}
      >
        <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
          <rect x="2" y="3" width="12" height="10" rx="1.5" fill="none" stroke="currentColor" stroke-width="1.3" />
          <line x1="8" y1="3" x2="8" y2="13" stroke="currentColor" stroke-width="1.3" />
        </svg>
      </button>
      <button
        class="ctl"
        title="split down ({KEYS.splitDown})"
        aria-label="split down"
        onclick={() => ctrl.splitPaneAt(node.id, "col")}
      >
        <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
          <rect x="2" y="3" width="12" height="10" rx="1.5" fill="none" stroke="currentColor" stroke-width="1.3" />
          <line x1="2" y1="8" x2="14" y2="8" stroke="currentColor" stroke-width="1.3" />
        </svg>
      </button>
      {#if !zoomed}
        <button class="ctl" title="zoom ({KEYS.zoom})" aria-label="zoom" onclick={() => ctrl.zoomPane(node.id)}>
          <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
            <path d="M9.5 2.5h4v4M6.5 13.5h-4v-4M13.5 2.5L9 7M2.5 13.5L7 9" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" />
          </svg>
        </button>
      {/if}
      <button
        class="ctl"
        title="close view ({KEYS.closeView})"
        aria-label="close view"
        onclick={() => ctrl.closeView(node.id)}
      >
        <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
          <path d="M4 4l8 8M12 4l-8 8" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" />
        </svg>
      </button>
    </div>

    {#if linkMenuOpen}
      <div class="link-menu" role="menu" aria-label="link to agent">
        <div class="link-menu-title">link to agent</div>
        {#each agentChoices as a (a.id)}
          <button class="link-menu-item" role="menuitem" onclick={() => chooseAgent(a.id)}>
            <span class="chip-dot menu-dot" style:--hue={agentHue(a.id)}></span>
            <span class="link-menu-name">{sessionLabel(a.id)}</span>
            {#if linkedAgentId === a.id}
              <span class="link-menu-state">linked · click to unlink</span>
            {/if}
          </button>
        {/each}
      </div>
    {/if}
  </div>
</div>

<style>
  .bar {
    flex: none;
    display: flex;
    align-items: stretch;
    height: 26px;
    overflow: hidden;
    border-bottom: 1px solid var(--edge);
    padding: 0 4px;
    user-select: none;
  }

  .tabs {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: stretch;
    overflow: hidden;
  }

  .tab {
    position: relative;
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 0 0.4rem 0 0.55rem;
    max-width: 200px;
    min-width: 0;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: default;
    user-select: none;
    transition: color 0.12s ease;
  }

  .tab:hover {
    color: var(--fg);
  }

  /* Active-tab emphasis via weight, not color — the bar stays quiet. */
  .tab.active {
    color: var(--fg);
    font-weight: 600;
  }

  /* Drag insertion caret. */
  .tab.insert::before,
  .tab-tail.insert::before {
    content: "";
    position: absolute;
    top: 5px;
    bottom: 5px;
    left: -1px;
    width: 2px;
    border-radius: 1px;
    background: var(--accent);
  }

  .tab-tail {
    position: relative;
    flex: 1;
    min-width: 8px;
  }

  /* Type glyph, shared slot for both surfaces; a terminal's glyph carries
     its session-state color (same palette as the rail dots). */
  .glyph {
    flex: none;
    color: var(--muted);
    opacity: 0.8;
  }

  .glyph.alive {
    color: var(--accent);
    opacity: 1;
  }

  .glyph.attn {
    color: var(--warn);
    opacity: 1;
  }

  .glyph.err {
    color: var(--err);
    opacity: 1;
  }

  .glyph.rate {
    color: var(--rate);
    opacity: 1;
  }

  .glyph.done {
    color: var(--fg);
    opacity: 0.6;
  }

  .glyph.starting {
    opacity: 0.9;
  }

  /* File type glyph slot; quiet by default, lifts to full strength on the
     active tab (parity with the weight-based active emphasis). */
  .tab-glyph {
    flex: none;
    display: flex;
    align-items: center;
    opacity: 0.85;
  }

  .tab-glyph.on {
    opacity: 1;
  }

  /* Unsaved-edits marker: sits in the glyph slot, same footprint as a glyph. */
  .dirty-dot {
    flex: none;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--accent);
  }

  .tab-name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .tab-close {
    appearance: none;
    border: none;
    background: none;
    padding: 0 0.15rem;
    font: inherit;
    font-size: var(--text-md);
    font-weight: 400;
    line-height: 1;
    color: var(--muted);
    cursor: pointer;
    opacity: 0;
    flex: none;
    transition:
      opacity 0.12s ease,
      color 0.12s ease;
  }

  .tab:hover .tab-close,
  .tab.active .tab-close {
    opacity: 0.7;
  }

  .tab-close:hover {
    opacity: 1;
    color: var(--fg);
  }

  /* --- linked-terminal chips ------------------------------------------- */

  .links {
    flex: none;
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 0 4px;
    min-width: 0;
    overflow: hidden;
  }

  /* One chip = the visible bond: agent hue, quiet until something happens. */
  .chip {
    display: flex;
    align-items: center;
    height: 18px;
    border: 1px solid hsl(var(--hue) 45% 55% / 0.45);
    border-radius: 9px;
    background: hsl(var(--hue) 50% 55% / 0.08);
    color: var(--fg);
    font-family: var(--mono);
    font-size: var(--text-xs);
    max-width: 150px;
    min-width: 0;
  }

  .chip-main {
    appearance: none;
    border: none;
    background: none;
    display: flex;
    align-items: center;
    gap: 5px;
    height: 100%;
    padding: 0 2px 0 7px;
    font: inherit;
    color: inherit;
    cursor: pointer;
    min-width: 0;
  }

  .chip-name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .chip-spark {
    flex: none;
    color: hsl(var(--hue) 55% 55%);
  }

  .chip-dot {
    flex: none;
    width: 7px;
    height: 7px;
    border-radius: 50%;
  }

  /* Chip dot states: what the linked shell is doing right now. */
  .chip-dot.ready {
    background: hsl(var(--hue) 40% 55% / 0.55);
  }

  .chip-dot.busy {
    background: var(--accent);
  }

  .chip-dot.quiet {
    background: none;
    border: 1px solid var(--muted);
    opacity: 0.6;
  }

  /* Agent exec queued: hollow agent-hue ring, waiting its turn. */
  .chip-dot.queued {
    background: none;
    border: 1.5px solid hsl(var(--hue) 60% 55%);
  }

  /* Agent exec running: the leash is being pulled — a gentle hue pulse. */
  .chip-dot.exec {
    background: hsl(var(--hue) 65% 55%);
    animation: chip-pulse 1.4s ease-in-out infinite;
  }

  @keyframes chip-pulse {
    0%,
    100% {
      box-shadow: 0 0 0 0 hsl(var(--hue) 65% 55% / 0.55);
    }
    50% {
      box-shadow: 0 0 0 3.5px hsl(var(--hue) 65% 55% / 0);
    }
  }

  .chip-x {
    appearance: none;
    border: none;
    background: none;
    padding: 0 6px 0 2px;
    font: inherit;
    font-size: var(--text-md);
    line-height: 1;
    color: var(--muted);
    cursor: pointer;
    opacity: 0;
    flex: none;
    transition:
      opacity 0.12s ease,
      color 0.12s ease;
  }

  .chip:hover .chip-x {
    opacity: 0.7;
  }

  .chip-x:hover {
    opacity: 1;
    color: var(--fg);
  }

  /* --- link-to-agent menu ------------------------------------------------ */

  .link-menu {
    position: absolute;
    top: 25px;
    right: 4px;
    z-index: 20;
    min-width: 180px;
    padding: 4px;
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 7px;
    box-shadow: 0 6px 24px rgba(0, 0, 0, 0.22);
  }

  .link-menu-title {
    padding: 3px 8px 5px;
    font-size: var(--text-xs);
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
  }

  .link-menu-item {
    appearance: none;
    border: none;
    background: none;
    display: flex;
    align-items: center;
    gap: 7px;
    width: 100%;
    padding: 4px 8px;
    border-radius: 4px;
    font: inherit;
    font-size: var(--text-sm);
    color: var(--fg);
    cursor: pointer;
    text-align: left;
    transition: background-color 0.12s ease;
  }

  .link-menu-item:hover {
    background: var(--row-hover);
  }

  .menu-dot {
    background: hsl(var(--hue) 55% 55%);
  }

  .link-menu-name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .link-menu-state {
    margin-left: auto;
    padding-left: 10px;
    font-size: var(--text-xs);
    color: var(--muted);
  }

  @media (prefers-reduced-motion: reduce) {
    .chip-dot.exec {
      animation: none;
    }
  }

  /* --- controls at the bar's right edge --- */

  .bar-right {
    position: relative;
    flex: none;
    display: flex;
    align-items: center;
    gap: 4px;
    padding-left: 4px;
  }

  .controls {
    display: flex;
    align-items: center;
    gap: 1px;
    opacity: 0;
    pointer-events: none;
    transition: opacity 0.12s ease;
  }

  .bar:hover .controls,
  .controls:focus-within {
    opacity: 1;
    pointer-events: auto;
  }

  .ctl {
    appearance: none;
    border: none;
    background: none;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 20px;
    height: 18px;
    padding: 0;
    border-radius: 4px;
    color: var(--muted);
    cursor: pointer;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .ctl:hover {
    background: var(--row-hover);
    color: var(--fg);
  }

  /* The link control stays lit while its menu is open (its mouse home). */
  .ctl.on {
    background: var(--row-hover);
    color: var(--fg);
  }

  /* Persistent while zoomed — the always-visible mouse exit from zoom. */
  .zoom-badge {
    appearance: none;
    border: none;
    font: inherit;
    font-size: var(--text-xs);
    letter-spacing: 0.09em;
    text-transform: uppercase;
    color: var(--muted);
    background: color-mix(in srgb, var(--fg) 7%, transparent);
    padding: 0 7px;
    height: 18px;
    border-radius: 4px;
    cursor: pointer;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .zoom-badge:hover {
    background: color-mix(in srgb, var(--fg) 12%, transparent);
    color: var(--fg);
  }
</style>
