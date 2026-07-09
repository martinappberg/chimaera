<script lang="ts">
  /**
   * The agent launcher popover (DESIGN.md "The agent launcher"), opened by
   * the split button's chevron ONLY — hover (~150ms) on the chevron or a
   * click; the main surface is a pure instant spawn and never opens this.
   *
   * One question: WHICH agent. One row per known CLI, every row carrying a
   * link to its official docs (opens in the browser):
   * - installed → click spawns; the row names whose binary runs — "yours"
   *   (your own on PATH) or "chimaera" (accent, a build chimaera installed
   *   under ~/.chimaera/agents) — with the version and the resolved path in
   *   the tooltip;
   * - installed but outdated (npm-era codex, pre-`codex login`) → click
   *   still spawns, an "update" chip runs the daemon's curated update
   *   in-app (managed runtimes), streaming into a visible terminal;
   * - not installed → muted, an "install" chip does the same for a fresh
   *   install. One explicit click, never silent: the tooltip says exactly
   *   what is fetched and where it lands. No curated install (gemini,
   *   phase 2: node runtime) → no chip at all; the docs link is the
   *   affordance — the POST would only 400.
   *
   * Resuming lives in the rail's RECENT section, not here.
   *
   * Keyboard: ArrowUp/Down moves the highlight, Enter activates (spawn or
   * install), Escape closes.
   */
  import { onMount } from "svelte";
  import { getAgentDefault, listAgents, type AgentInfo, type LaunchPick } from "./launcher";
  import SessionGlyph from "../shared/SessionGlyph.svelte";

  interface Props {
    /** The split button's rect; the popover hangs below it (fixed). */
    anchor: DOMRect;
    onPick: (pick: LaunchPick) => void;
    onInstall: (agent: AgentInfo) => void;
    onClose: () => void;
    /** Report the freshly-probed catalog up so the split button reflects it
     *  (this popover always re-detects on open). */
    onAgents?: (agents: AgentInfo[]) => void;
  }

  let { anchor, onPick, onInstall, onClose, onAgents }: Props = $props();

  let agents = $state<AgentInfo[] | null>(null);
  let loadError = $state<string | null>(null);
  /** Loading is invisible <150ms, then a soft pulse (polish inventory). */
  let showLoading = $state(false);

  let hl = $state(0);
  let rootEl = $state<HTMLElement | null>(null);

  /** The bare version number, wherever the CLI buried it ("codex-cli 0.52.0",
   *  "2.1.202 (Claude Code)"). */
  function versionNumber(version: string): string {
    return version.split(" ").find((t) => /^\d/.test(t)) ?? version.split(" ")[0];
  }

  /** Version-tag tooltip: whose binary this is and where it resolves — the
   *  answer to "chimaera's install or mine?" the rail alone can't give. */
  function whereTitle(a: AgentInfo): string {
    const whose = a.managed
      ? "installed by chimaera in ~/.chimaera/agents"
      : a.path
        ? `your own install — ${a.path}`
        : "your own install on PATH";
    return a.version !== null ? `${a.version} — ${whose}` : whose;
  }

  onMount(() => {
    const def = getAgentDefault();
    const loadTimer = setTimeout(() => (showLoading = true), 150);
    void listAgents()
      .then((a) => {
        agents = a;
        onAgents?.(a);
        hl = Math.max(
          0,
          a.findIndex((x) => x.id === def.agent),
        );
        // The detection cache is daemon-lifetime, so an install/update made
        // since (field report: codex updated, chip still said "update")
        // would never surface. Show the cached rows instantly, then
        // re-detect in the background and swap in the truth.
        return listAgents(true).then((fresh) => {
          if (fresh.length > 0) {
            agents = fresh;
            onAgents?.(fresh);
          }
        });
      })
      .catch((e) => {
        if (agents === null) {
          loadError = e instanceof Error ? e.message : "failed to load agents";
        }
        // refresh failures keep the cached rows — never blank a shown list
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

  function activate(i: number): void {
    const a = agents?.[i];
    if (a === undefined) return;
    if (!a.installed) {
      // No curated install: nothing to run — the docs link is the
      // affordance (the POST would 400).
      if (a.managedInstall) onInstall(a);
      return;
    }
    onPick({ agent: a.id });
  }

  function move(delta: number): void {
    const n = agents?.length ?? 0;
    if (n === 0) return;
    hl = (hl + delta + n) % n;
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
        <!-- div, not button: rows contain a real link (docs) and a real
             button (update chip); interactive elements cannot nest. The
             popover root owns all keyboard handling (roving highlight). -->
        <div
          class="arow"
          class:hl={hl === i}
          class:missing={!a.installed}
          role="menuitem"
          tabindex="-1"
          data-item={i}
          onpointerenter={() => (hl = i)}
          onclick={() => activate(i)}
          onkeydown={(e) => {
            // Direct-focus semantics for when focus lands inside a row
            // (e.g. after clicking the docs link). stopPropagation is
            // load-bearing: the popover root also handles Enter, and a
            // bubbled event would activate twice — two spawned sessions.
            if (e.key === "Enter") {
              e.preventDefault();
              e.stopPropagation();
              activate(i);
            }
          }}
        >
          <span class="gslot"><SessionGlyph kind="agent" agentKind={a.id} size={13} /></span>
          <span class="aname">{a.name}</span>
          {#if a.installUrl !== null}
            <!-- Official docs, in the browser — quiet until the row is hot. -->
            <a
              class="adocs"
              href={a.installUrl}
              target="_blank"
              rel="noreferrer"
              title="open the official docs"
              tabindex="-1"
              onclick={(e) => e.stopPropagation()}>docs&thinsp;↗</a
            >
          {/if}
          {#if !a.installed && a.managedInstall}
            <button
              class="achip"
              tabindex="-1"
              title="downloads the official {a.name} build into ~/.chimaera/agents — runs in a terminal you can watch"
              onclick={(e) => {
                e.stopPropagation();
                onInstall(a);
              }}
            >
              install
            </button>
          {:else if a.outdated}
            <button
              class="achip"
              tabindex="-1"
              title="this build is too old to sign in — downloads the official {a.name} build into ~/.chimaera/agents — runs in a terminal you can watch"
              onclick={(e) => {
                e.stopPropagation();
                onInstall(a);
              }}
            >
              update
            </button>
          {:else if a.installed}
            <!-- Provenance in words, not just a color: "chimaera" = a build
                 chimaera installed itself (~/.chimaera/agents), "yours" = your
                 own on PATH — the launcher's answer to "whose claude runs?".
                 The version number follows when the probe caught it; the
                 tooltip carries the resolved path. -->
            <span class="aver" class:managed={a.managed} title={whereTitle(a)}>
              <span class="prov">{a.managed ? "chimaera" : "yours"}</span>
              {#if a.version !== null}<span class="num">{versionNumber(a.version)}</span>{/if}
            </span>
          {/if}
        </div>
      {/each}
    </div>

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

  /* Official-docs link: invisible weight until the row is hot. */
  .adocs {
    flex: none;
    font-size: var(--text-xs);
    color: var(--muted);
    text-decoration: none;
    opacity: 0;
    transition:
      opacity 0.12s ease,
      color 0.12s ease;
  }

  .arow.hl .adocs,
  .arow:hover .adocs {
    opacity: 0.8;
  }

  .adocs:hover {
    color: var(--fg);
    opacity: 1;
    text-decoration: underline;
  }

  .aver {
    flex: none;
    display: inline-flex;
    align-items: baseline;
    gap: 5px;
    max-width: 150px;
    overflow: hidden;
    white-space: nowrap;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }

  /* Provenance word ("chimaera" / "yours") — the at-a-glance answer to whose
     binary a spawn runs. Muted for the user's own; accent tint when chimaera
     installed the build itself (~/.chimaera/agents). */
  .prov {
    flex: none;
  }

  .aver.managed .prov {
    color: var(--accent);
  }

  /* Version number, set off from the provenance word by a thin middot. */
  .num {
    flex: none;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .num::before {
    content: "·";
    margin-right: 5px;
    opacity: 0.5;
  }

  /* Uninstalled agents: visible but muted — honest, not hidden. */
  .arow.missing .aname {
    color: var(--muted);
    font-weight: 400;
  }

  .arow.missing :global(.sglyph) {
    opacity: 0.55;
  }

  /* install / update chip: the one explicit click that runs the daemon's
     curated command — visibly, in a terminal pane (managed runtimes). */
  .achip {
    appearance: none;
    background: none;
    font: inherit;
    flex: none;
    padding: 1px 8px;
    border: 1px solid var(--edge);
    border-radius: 10px;
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: pointer;
    transition:
      color 0.12s ease,
      border-color 0.12s ease,
      transform 0.08s ease;
  }

  .arow.hl .achip,
  .arow:hover .achip {
    color: var(--fg);
    border-color: color-mix(in srgb, var(--fg) 30%, transparent);
  }

  .achip:active {
    transform: translateY(0.5px);
  }

  .achip:focus-visible {
    outline: 2px solid var(--focus-ring);
    outline-offset: 1px;
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
