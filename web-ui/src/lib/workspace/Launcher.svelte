<script lang="ts">
  /**
   * The agent launcher popover (docs/design/README.md "The agent launcher"), opened by
   * the rail's chevron ONLY — hover (~150ms) or a click; the rail's quick
   * starts are pure instant spawns and never open this.
   *
   * The rail already starts Claude Code and Codex (and the saved default) in
   * one click, so this answers the rest of "which agent, and how":
   * - the quick agents first, in rail order — here for their terminal start
   *   (the icon button, or ⌘↵) and update affordance;
   * - then "other agents" (Antigravity, Grok Build, Cursor CLI, extension
   *   agents), ready chats first, each showing its install state: the
   *   version when installed, "not installed" with an install chip (a
   *   curated managed install) or a set-up chip into Settings → Agents;
   * - a footer link into Settings → Agents, where every agent is managed.
   *
   * Installed rows read as just a version; whose binary it is ("installed by
   * chimaera" vs your own, with the resolved path) is the version's tooltip.
   * A strictly newer known release grows "→ <new>": a one-click curated
   * update for a chimaera-managed build, quiet information for your own.
   * Every row links its official docs when hot.
   *
   * Picking here makes that agent the saved default (the rail's primary).
   * Resuming lives in the rail's RECENT section, not here.
   *
   * Keyboard: the root holds focus with a roving highlight
   * (aria-activedescendant). ArrowUp/Down (Home/End) move across the agent
   * rows and the settings link, Enter activates (start, install, or open
   * settings), ⌘/Ctrl+Enter starts in the terminal, Escape closes and hands
   * focus back to the chevron.
   */
  import { onMount } from "svelte";
  import {
    getAgentDefault,
    groupAgents,
    listAgents,
    versionNumber,
    type AgentInfo,
    type LaunchPick,
  } from "./launcher";
  import { isMac } from "../shared/keys";
  import { openInSystemBrowser } from "../shared/urlOpen";
  import SessionGlyph from "../shared/SessionGlyph.svelte";

  interface Props {
    /** The rail's quick-start group rect; the popover hangs below it (fixed). */
    anchor: DOMRect;
    onPick: (pick: LaunchPick) => void;
    onInstall: (agent: AgentInfo) => void;
    /** Update a chimaera-MANAGED binary to `latestVersion` (the daemon's
     *  curated script in a visible pane). Only reachable when the row is
     *  managed AND a strictly newer release is known. */
    onUpdate: (agent: AgentInfo) => void;
    /** Escape passes "escape" so focus returns to the chevron (menu-button
     *  pattern); an outside press leaves focus where it landed. */
    onClose: (how?: "escape") => void;
    /** Open Settings → Agents (the footer link, and the set-up path for an
     *  agent chimaera has no curated install for). */
    onSettings: () => void;
    /** Report the freshly-probed catalog up so the rail reflects it
     *  (this popover always re-detects on open). */
    onAgents?: (agents: AgentInfo[]) => void;
    /** The window's last-known catalog (App keeps it from boot + installs).
     *  Rows paint from it INSTANTLY — no "checking…" flash on every open —
     *  and the background re-detect swaps in the truth. */
    initial?: AgentInfo[] | null;
  }

  let { anchor, onPick, onInstall, onUpdate, onClose, onSettings, onAgents, initial = null }: Props = $props();

  /** Read once per open: the default decides which agents are "quick". */
  const defaultId = getAgentDefault().agent;

  let catalog = $state<AgentInfo[] | null>(null);
  let loadError = $state<string | null>(null);
  /** Loading is invisible <150ms, then a soft pulse (polish inventory). */
  let showLoading = $state(false);

  const groups = $derived(catalog === null ? null : groupAgents(catalog, defaultId));
  /** The roving order: quick agents, others, then the settings link. */
  const rows = $derived(groups === null ? [] : [...groups.quick, ...groups.others]);
  const quickCount = $derived(groups?.quick.length ?? 0);
  const settingsIndex = $derived(rows.length);

  let hl = $state(0);
  let rootEl = $state<HTMLElement | null>(null);

  /** Version tooltip: whose binary this is and where it resolves — the
   *  answer to "chimaera's install or mine?" the row alone doesn't spell out. */
  function whereTitle(a: AgentInfo): string {
    const whose = a.managed
      ? "installed by chimaera in ~/.chimaera/agents"
      : a.path
        ? `your own install — ${a.path}`
        : "your own install on PATH";
    return a.version !== null ? `${a.version} — ${whose}` : whose;
  }

  /** The row's one quiet state word: a version when it can run, else what it needs. */
  function stateLine(a: AgentInfo): string {
    if (!a.installed) return "not installed";
    if (a.outdated) return "needs an update";
    if (a.chatSetupRequired) return "chat needs setup";
    return a.version !== null ? versionNumber(a.version) : "installed";
  }

  /** Swap in a fresh catalog, keeping the highlight on the same agent (or on
   *  the settings link) rather than the same index. */
  function adopt(next: AgentInfo[]): void {
    const onSettingsRow = catalog !== null && hl === settingsIndex;
    const selected = rows[hl]?.id;
    catalog = next;
    if (onSettingsRow) hl = rows.length;
    else hl = Math.max(0, rows.findIndex((a) => a.id === selected));
  }

  onMount(() => {
    // The window's last-known catalog paints the rows synchronously; only a
    // cold window (boot probe never landed) fetches before first paint. The
    // highlight starts on the default, which leads the quick group.
    if (initial !== null && initial.length > 0) catalog = initial;
    const loadTimer = setTimeout(() => (showLoading = true), 150);
    // The detection cache is daemon-lifetime, so an install/update made
    // since (field report: codex updated, chip still said "update") would
    // never surface. Whatever painted first — the prop or the daemon's
    // cached rows — a background re-detect swaps in the truth.
    const first = catalog !== null ? Promise.resolve(catalog) : listAgents();
    void first
      .then((a) => {
        if (catalog === null) {
          catalog = a;
          onAgents?.(a);
        }
        return listAgents(true).then((fresh) => {
          if (fresh.length > 0) {
            adopt(fresh);
            onAgents?.(fresh);
          }
        });
      })
      .catch((e) => {
        if (catalog === null) {
          loadError = e instanceof Error ? e.message : "failed to load agents";
        }
        // refresh failures keep the shown rows — never blank a shown list
      })
      .finally(() => clearTimeout(loadTimer));

    rootEl?.focus();

    // Overlay language: any outside press or Escape closes. Presses on the
    // rail's quick-start group are its own toggle/spawn semantics — not
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
    // Wide enough that a hot row keeps name, version, the "→ <new>" update
    // affordance, the docs link and both actions un-truncated.
    const width = 320;
    const left = Math.max(8, Math.min(anchor.left, viewW - width - 8));
    const top = anchor.bottom + 6;
    const maxH = Math.max(180, viewH - top - 12);
    return { left, top, width, maxH };
  });

  /** Row/Enter must do what the visible primary action says. The separate
   *  terminal action stays explicit; the rail's quick starts use the saved
   *  default view. */
  function activate(i: number, ui: "chat" | "term" = "chat"): void {
    if (i === settingsIndex) {
      onSettings();
      return;
    }
    const a = rows[i];
    if (a === undefined) return;
    if (!a.installed || (a.chatSetupRequired && ui !== "term")) {
      // No curated install: Settings → Agents is where it gets set up.
      if (a.managedInstall) onInstall(a);
      else onSettings();
      return;
    }
    if (!a.chatCapable) {
      // Always a TUI, but NOT an explicit pick — launching a non-chat agent
      // must not silently change the default for chat-capable ones.
      onPick({ agent: a.id, ui: "term", explicit: false });
      return;
    }
    onPick({ agent: a.id, ui, explicit: true });
  }

  function move(delta: number): void {
    const n = rows.length + 1;
    if (catalog === null) return;
    hl = (hl + delta + n) % n;
  }

  function onKeydown(e: KeyboardEvent): void {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      onClose("escape");
      return;
    }
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      move(e.key === "ArrowDown" ? 1 : -1);
      return;
    }
    if (e.key === "Home" || e.key === "End") {
      e.preventDefault();
      if (catalog !== null) hl = e.key === "Home" ? 0 : settingsIndex;
      return;
    }
    if (e.key === "Enter") {
      e.preventDefault();
      if (catalog !== null) activate(hl, e.metaKey || e.ctrlKey ? "term" : undefined);
    }
  }

  const itemId = (i: number) => `agent-launcher-item-${i}`;
</script>

{#snippet agentRow(a: AgentInfo, i: number)}
  <!-- div, not button: rows contain a real link (docs) and real buttons
       (chips); interactive elements cannot nest. The popover root owns all
       keyboard handling (roving highlight). -->
  <div
    class="arow"
    class:hl={hl === i}
    class:missing={!a.installed}
    role="menuitem"
    tabindex="-1"
    id={itemId(i)}
    data-agent={a.id}
    onpointerenter={() => (hl = i)}
    onclick={() => activate(i)}
    onkeydown={(e) => {
      // Direct-focus semantics for when focus lands inside a row (e.g.
      // after clicking the docs link). stopPropagation is load-bearing: the
      // popover root also handles Enter, and a bubbled event would activate
      // twice — two spawned sessions.
      if (e.key === "Enter") {
        e.preventDefault();
        e.stopPropagation();
        activate(i, e.metaKey || e.ctrlKey ? "term" : undefined);
      }
    }}
  >
    <span class="gslot"><SessionGlyph kind="agent" agentKind={a.id} size={13} /></span>
    <span class="aname">{a.name}</span>
    <span class="asub">
      <span class="aver" class:managed={a.managed && a.installed} title={a.installed ? whereTitle(a) : undefined}>{stateLine(a)}</span>
      {#if a.installed && !a.outdated && a.updateAvailable && a.latestVersion !== null}
        {#if a.managed}
          <!-- One-click update of chimaera's own install: the same curated,
               checksum-verified script as install, atomic swap. -->
          <button
            class="aup"
            tabindex="-1"
            title="update to {a.latestVersion} — downloads the official {a.name} build into ~/.chimaera/agents, with progress and results in agent setup"
            onclick={(e) => {
              e.stopPropagation();
              onUpdate(a);
            }}>→&thinsp;{a.latestVersion}</button
          >
        {:else}
          <!-- The user's own binary: chimaera never touches it — the newer
               release is information, docs is the way. -->
          <span class="aup info" title="{a.latestVersion} is out — this is your own install; update it your way (docs ↗)"
            >→&thinsp;{a.latestVersion}</span
          >
        {/if}
      {/if}
      {#if a.installUrl !== null}
        <!-- Official docs, in the browser — quiet until the row is hot. -->
        <a
          class="adocs"
          href={a.installUrl}
          target="_blank"
          rel="noopener noreferrer"
          title="open the official docs"
          tabindex="-1"
          onclick={(e) => {
            // Through the shell: in the app a _blank navigation is
            // swallowed by the window's origin guard.
            e.stopPropagation();
            e.preventDefault();
            openInSystemBrowser(a.installUrl ?? "");
          }}>docs&thinsp;↗</a
        >
      {/if}
    </span>
    {#if !a.installed || a.chatSetupRequired || a.outdated}
      <!-- The one explicit click that unblocks it: a curated install/update
           (visible progress in agent setup), or Settings → Agents when
           chimaera has no curated install for it. -->
      <button
        class="achip"
        class:solid={i < quickCount}
        tabindex="-1"
        title={a.managedInstall
          ? `downloads the official ${a.name} build into ~/.chimaera/agents — runs with progress and results in agent setup`
          : `set up ${a.name} in Settings → Agents`}
        onclick={(e) => {
          e.stopPropagation();
          if (a.managedInstall) onInstall(a);
          else onSettings();
        }}
      >
        {!a.managedInstall ? "set up" : a.outdated ? "update" : a.chatSetupRequired ? "set up chat" : "install"}
      </button>
    {/if}
    {#if a.installed && (!a.chatSetupRequired || a.chatCapable)}
      <!-- The two ways in: "chat" (and the whole row, and ↵) is the
           structured chat UI — the default; the terminal button (⌘↵) is the
           explicit TUI path. No chat view → one "terminal" button. -->
      {#if a.chatCapable}
        <button
          class="tbtn"
          tabindex="-1"
          aria-label="open {a.name} in the terminal"
          data-tip="terminal · {isMac ? '⌘' : 'ctrl'}↵"
          onclick={(e) => {
            e.stopPropagation();
            activate(i, "term");
          }}
        >
          <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true">
            <rect x="1.5" y="2.5" width="13" height="11" rx="2" fill="none" stroke="currentColor" stroke-width="1.2" />
            <path
              d="M4.2 6l2.4 2-2.4 2"
              fill="none"
              stroke="currentColor"
              stroke-width="1.3"
              stroke-linecap="round"
              stroke-linejoin="round"
            />
            <path d="M8.4 10.6h3.2" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" />
          </svg>
        </button>
      {/if}
      {#if !a.chatSetupRequired && !a.outdated}
        <button
          class="obtn"
          tabindex="-1"
          title={a.chatCapable ? "open in the chat view (↵)" : `opens in the terminal — ${a.name} has no chat view`}
          onclick={(e) => {
            e.stopPropagation();
            activate(i, a.chatCapable ? "chat" : "term");
          }}
        >
          {a.chatCapable ? "chat" : "terminal"}
        </button>
      {/if}
    {/if}
  </div>
{/snippet}

<div
  class="launcher"
  role="menu"
  aria-label="new agent"
  tabindex="-1"
  aria-activedescendant={catalog !== null ? itemId(hl) : undefined}
  bind:this={rootEl}
  style:left="{pos.left}px"
  style:top="{pos.top}px"
  style:width="{pos.width}px"
  style:max-height="{pos.maxH}px"
  onkeydown={onKeydown}
>
  {#if groups === null}
    {#if loadError !== null}
      <div class="state err">{loadError}</div>
    {:else if showLoading}
      <div class="state pulse">checking installed agents…</div>
    {/if}
  {:else}
    <div class="agents">
      {#each groups.quick as a, i (a.id)}
        {@render agentRow(a, i)}
      {/each}
      {#if groups.others.length > 0}
        <div class="group-label" role="presentation">other agents</div>
        {#each groups.others as a, j (a.id)}
          {@render agentRow(a, quickCount + j)}
        {/each}
      {/if}
    </div>

    <div class="foot">
      <span class="hint"><kbd>{isMac ? "⌘" : "ctrl"}↵</kbd> terminal</span>
      <button
        class="settings"
        class:hl={hl === settingsIndex}
        role="menuitem"
        tabindex="-1"
        id={itemId(settingsIndex)}
        onpointerenter={() => (hl = settingsIndex)}
        onclick={onSettings}>agent settings <span aria-hidden="true">→</span></button
      >
    </div>
  {/if}
</div>

<style>
  /* The quiet seam between the quick agents and the rest: a hairline and a
     lowercase word, no chrome. */
  .group-label {
    margin: 4px 9px 0;
    padding: 7px 0 3px;
    border-top: 1px solid var(--edge);
    color: var(--muted);
    font-size: var(--text-xs);
    user-select: none;
  }
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
    padding: 6px 9px;
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

  /* One line per agent: name, then its quiet state (version / what it
     needs), then the actions. */
  .aname {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 500;
  }

  /* The state line: version (provenance in its tooltip) · update · docs. */
  .asub {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: baseline;
    gap: 7px;
    overflow: hidden;
    white-space: nowrap;
    font-size: var(--text-xs);
    color: var(--muted);
  }

  /* Official-docs link: invisible weight until the row is hot. */
  .adocs {
    flex: none;
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
    max-width: 110px;
    overflow: hidden;
    white-space: nowrap;
    font-family: var(--mono);
  }

  /* Update-available: "→ <new>" appended to the version line, only when a
     strictly newer release is known. A real button (accent) when chimaera
     manages the binary — one click runs the curated update in a setup
     pane; quiet muted information when the binary is the user's own
     (chimaera never touches an install it doesn't own). */
  .aup {
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    flex: none;
    font: inherit;

    color: var(--accent);
    opacity: 0.85;
    transition: opacity 0.12s ease;
  }

  button.aup {
    cursor: pointer;
  }

  button.aup:hover {
    opacity: 1;
    text-decoration: underline;
  }

  button.aup:focus-visible {
    outline: 2px solid var(--focus-ring);
    outline-offset: 1px;
  }

  .aup.info {
    color: var(--muted);
    opacity: 0.8;
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

  .achip.solid {
    color: var(--accent);
    border-color: color-mix(in srgb, var(--accent) 40%, transparent);
  }

  .achip:active {
    transform: translateY(0.5px);
  }

  .achip:focus-visible {
    outline: 2px solid var(--focus-ring);
    outline-offset: 1px;
  }

  /* The two ways in, quiet until the row is hot: "open" (the default — the
     chat view) and the terminal-icon button (the agent's own TUI). */
  .obtn,
  .tbtn {
    appearance: none;
    background: none;
    font: inherit;
    flex: none;
    border: 1px solid var(--edge);
    color: var(--muted);
    cursor: pointer;
    opacity: 0;
    transition:
      opacity 0.12s ease,
      color 0.12s ease,
      border-color 0.12s ease,
      transform 0.08s ease;
  }

  .arow.hl .obtn,
  .arow:hover .obtn,
  .arow.hl .tbtn,
  .arow:hover .tbtn {
    opacity: 1;
  }

  .obtn {
    padding: 2px 10px;
    border-radius: 10px;
    font-size: var(--text-xs);
    color: var(--fg);
    border-color: color-mix(in srgb, var(--fg) 25%, transparent);
  }

  .obtn:hover {
    color: var(--accent);
    border-color: color-mix(in srgb, var(--accent) 45%, transparent);
  }

  .tbtn {
    position: relative;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    border-radius: 6px;
  }

  .tbtn:hover {
    color: var(--fg);
    border-color: color-mix(in srgb, var(--fg) 30%, transparent);
  }

  /* The delayed tooltip: nothing on a pass-through hover, fades in after
     ~0.5s of intent. Pure CSS off data-tip; pointer-events off so it never
     steals the hover it explains. */
  .tbtn::after {
    content: attr(data-tip);
    position: absolute;
    top: calc(100% + 7px);
    right: -2px;
    z-index: 5;
    padding: 3px 9px;
    white-space: nowrap;
    font-size: var(--text-xs);
    color: var(--fg);
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 6px;
    box-shadow: 0 4px 14px rgba(0, 0, 0, 0.18);
    opacity: 0;
    pointer-events: none;
    transition: opacity 0.12s ease;
  }

  .tbtn:hover::after {
    opacity: 1;
    transition-delay: 0.5s;
  }

  .obtn:active,
  .tbtn:active {
    transform: translateY(0.5px);
  }

  .foot {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-top: 5px;
    padding: 5px 3px 0 9px;
    border-top: 1px solid var(--edge);
    font-size: var(--text-xs);
    color: var(--muted);
    user-select: none;
  }

  .foot kbd {
    font-family: var(--mono);
    font-size: var(--text-xs);
    padding: 0 0.25rem;
    border: 1px solid var(--edge);
    border-radius: 4px;
  }

  /* Settings → Agents: the way into everything this menu doesn't do. */
  .settings {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    padding: 4px 6px;
    border-radius: 6px;
    cursor: pointer;
    transition:
      color 0.12s ease,
      background-color 0.12s ease;
  }

  .settings.hl {
    color: var(--fg);
    background: var(--row-hover);
  }
</style>
