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
   * Docs live in Settings → Agents, not on the rows.
   *
   * Picking here makes that agent the saved default (the rail's primary).
   * Resuming lives in the rail's RECENT section, not here.
   *
   * In a git workspace the footer also holds one quiet switch: new agents
   * start in their own worktree (a fresh branch). Remembered per
   * workspace; every new-agent path follows it, not only this menu. It
   * joins the roving order between the agents and the settings link.
   *
   * Keyboard: the root holds focus with a roving highlight
   * (aria-activedescendant). ArrowUp/Down (Home/End) move across the agent
   * rows, the worktree switch and the settings link, Enter activates (start,
   * install, flip the switch, or open settings), ⌘/Ctrl+Enter starts in the
   * terminal, Escape closes and hands focus back to the chevron.
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
  import SessionGlyph from "../shared/SessionGlyph.svelte";
  import Chevron from "../shared/Chevron.svelte";

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
    /** The workspace's "new agents start in a new worktree" choice; null
     *  hides the switch (not a git repository). */
    inWorktree?: boolean | null;
    onToggleWorktree?: (next: boolean) => void;
  }

  let {
    anchor,
    onPick,
    onInstall,
    onUpdate,
    onClose,
    onSettings,
    onAgents,
    initial = null,
    inWorktree = null,
    onToggleWorktree,
  }: Props = $props();

  /** Read once per open: the default decides which agents are "quick". */
  const defaultId = getAgentDefault().agent;

  let catalog = $state<AgentInfo[] | null>(null);
  let loadError = $state<string | null>(null);
  /** Loading is invisible <150ms, then a soft pulse (polish inventory). */
  let showLoading = $state(false);

  const groups = $derived(catalog === null ? null : groupAgents(catalog, defaultId));
  const quickCount = $derived(groups?.quick.length ?? 0);
  const hasOthers = $derived((groups?.others.length ?? 0) > 0);
  /** The other agents stay folded behind one row until asked for — the
   *  menu is "which of the two" most days, not a catalog. Remembered. */
  const OTHERS_KEY = "chimaera.launcherOthersOpen";
  let othersOpen = $state(readOthersOpen());
  function readOthersOpen(): boolean {
    try {
      return localStorage.getItem(OTHERS_KEY) === "1";
    } catch {
      return false;
    }
  }
  function setOthersOpen(open: boolean): void {
    othersOpen = open;
    try {
      localStorage.setItem(OTHERS_KEY, open ? "1" : "0");
    } catch {
      // Per-viewer convenience only.
    }
  }
  const shownOthers = $derived(hasOthers && othersOpen ? (groups?.others ?? []) : []);
  /** The roving order: quick agents, the "other agents" row, the others
   *  while unfolded, the worktree switch (git workspaces only), then the
   *  settings link. */
  const othersIndex = $derived(hasOthers ? quickCount : -1);
  const agentRowCount = $derived(quickCount + (hasOthers ? 1 : 0) + shownOthers.length);
  const wtIndex = $derived(inWorktree !== null ? agentRowCount : -1);
  const settingsIndex = $derived(agentRowCount + (inWorktree !== null ? 1 : 0));
  /** The agent at roving index `i`, null for the non-agent rows. */
  function rowAt(i: number): AgentInfo | null {
    if (groups === null) return null;
    if (i < quickCount) return groups.quick[i] ?? null;
    if (!hasOthers || i === othersIndex) return null;
    return shownOthers[i - quickCount - 1] ?? null;
  }
  function indexOfAgent(id: string | undefined): number {
    if (groups === null || id === undefined) return -1;
    const q = groups.quick.findIndex((a) => a.id === id);
    if (q !== -1) return q;
    const o = shownOthers.findIndex((a) => a.id === id);
    return o === -1 ? -1 : quickCount + 1 + o;
  }
  function toggleOthers(): void {
    setOthersOpen(!othersOpen);
    if (hl > othersIndex && hl < agentRowCount) hl = othersIndex;
  }

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
   *  the same footer control) rather than the same index. */
  function adopt(next: AgentInfo[]): void {
    const onSettingsRow = catalog !== null && hl === settingsIndex;
    const onWtRow = catalog !== null && wtIndex !== -1 && hl === wtIndex;
    const onOthersRow = catalog !== null && hl === othersIndex;
    const selected = rowAt(hl)?.id;
    catalog = next;
    if (onSettingsRow) hl = settingsIndex;
    else if (onWtRow) hl = wtIndex;
    else if (onOthersRow) hl = othersIndex;
    else hl = Math.max(0, indexOfAgent(selected));
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
    if (i === wtIndex) {
      // A remembered choice, not a launch: the menu stays open.
      onToggleWorktree?.(!inWorktree);
      return;
    }
    if (i === othersIndex) {
      toggleOthers();
      return;
    }
    const a = rowAt(i);
    if (a === null) return;
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
    const n = settingsIndex + 1;
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
      <!-- The row (and ↵) opens the chat; the one hover-only glyph (⌘↵)
           is the explicit terminal path. An agent with no chat view opens
           in the terminal from the row itself. -->
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
            <rect x="1.5" y="2.5" width="13" height="11" rx="2" fill="none" stroke="currentColor" stroke-width="1.4" />
            <path
              d="M4.2 6l2.4 2-2.4 2"
              fill="none"
              stroke="currentColor"
              stroke-width="1.4"
              stroke-linecap="round"
              stroke-linejoin="round"
            />
            <path d="M8.4 10.6h3.2" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" />
          </svg>
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
      {#if hasOthers}
        <!-- The rest of the catalog, one row until asked: the menu is
             "which of the two" most days. -->
        <div
          class="arow others"
          class:hl={hl === othersIndex}
          role="menuitem"
          tabindex="-1"
          id={itemId(othersIndex)}
          aria-expanded={othersOpen}
          onpointerenter={() => (hl = othersIndex)}
          onclick={toggleOthers}
          onkeydown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              e.stopPropagation();
              toggleOthers();
            }
          }}
        >
          <span class="gslot others-chev"><Chevron open={othersOpen} /></span>
          <span class="aname others-label">other agents</span>
          <span class="asub"><span class="aver">{groups.others.length}</span></span>
        </div>
        {#each shownOthers as a, j (a.id)}
          {@render agentRow(a, quickCount + 1 + j)}
        {/each}
      {/if}
    </div>

    <div class="foot">
      {#if inWorktree !== null}
        <!-- The worktree choice lives in the footer, a peer of the settings
             link: a remembered setting, not a step. A focused switch's own
             Enter/Space must not also reach the root's Enter (a second flip). -->
        <button
          class="wt"
          class:hl={hl === wtIndex}
          role="menuitemcheckbox"
          aria-checked={inWorktree}
          tabindex="-1"
          id={itemId(wtIndex)}
          title={inWorktree
            ? "new agents start on a fresh branch in their own worktree"
            : "start new agents on a fresh branch in their own worktree"}
          onpointerenter={() => (hl = wtIndex)}
          onclick={() => activate(wtIndex)}
          onkeydown={(e) => {
            if (e.key === "Enter" || e.key === " ") e.stopPropagation();
          }}
        >
          <svg viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
            <path
              d="M5 3v7.5M5 12.5v.5M11 3v3a2.5 2.5 0 0 1-2.5 2.5H5"
              fill="none"
              stroke="currentColor"
              stroke-width="1.4"
              stroke-linecap="round"
            />
            <circle cx="5" cy="12.6" r="1.6" fill="none" stroke="currentColor" stroke-width="1.4" />
            <circle cx="5" cy="2.4" r="1.6" fill="none" stroke="currentColor" stroke-width="1.4" />
            <circle cx="11" cy="2.4" r="1.6" fill="none" stroke="currentColor" stroke-width="1.4" />
          </svg>
          <span class="wtlabel">In a new worktree</span>
          <span class="wtswitch" class:on={inWorktree} aria-hidden="true"><span class="wtknob"></span></span>
        </button>
      {:else}
        <!-- Outside a repository the footer keeps its terminal hint (the
             terminal button's tooltip says it too). -->
        <span class="hint"><kbd>{isMac ? "⌘" : "ctrl"}↵</kbd> terminal</span>
      {/if}
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
  /* The fold row: the rest of the catalog behind one muted row in the
     agents' own row language. No hairline of its own — the footer's is the
     menu's one seam, and two a row apart read as chrome. */
  .arow.others {
    margin-top: 2px;
    color: var(--muted);
    user-select: none;
  }
  .arow.others .others-label {
    font-weight: 400;
    color: var(--muted);
  }
  .arow.others.hl .others-label,
  .arow.others:hover .others-label {
    color: var(--fg);
  }
  .others-chev {
    display: inline-flex;
    align-items: center;
    justify-content: center;
  }
  .launcher {
    position: fixed;
    z-index: 120;
    display: flex;
    flex-direction: column;
    padding: 5px;
    /* The app's popover surface (app.css .overlay-surface): one radius and
       shadow for every menu that hangs off the chrome. */
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 8px;
    box-shadow: 0 6px 24px rgba(0, 0, 0, 0.22);
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
    border-radius: 5px; /* the rail's row radius */
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
  /* The one trailing control: a glyph that fades in with the row, no
     border — the row is the button, this is its alternative. */
  .tbtn {
    appearance: none;
    background: none;
    font: inherit;
    flex: none;
    border: none;
    color: var(--muted);
    cursor: pointer;
    opacity: 0;
    transition:
      opacity 0.12s ease,
      color 0.12s ease,
      background-color 0.12s ease,
      transform 0.08s ease;
  }

  /* Faint on a hot row: the row is the action, this glyph the alternative
     — it must not read as the row's one button. Full strength under the
     pointer itself. */
  .arow.hl .tbtn,
  .arow:hover .tbtn {
    opacity: 0.55;
  }

  .tbtn {
    position: relative;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    border-radius: 5px;
  }

  .tbtn:hover {
    opacity: 1;
    color: var(--fg);
    background: color-mix(in srgb, var(--fg) 8%, transparent);
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

  .tbtn:active {
    transform: translateY(0.5px);
  }

  /* The worktree switch: a footer control in the settings link's language
     (same padding, radius and hot state), the Switch recipe at a smaller
     size after its label. Both footer controls take the rows' side padding,
     so their hot boxes sit flush with the rows' above. */
  .wt {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-xs);
    min-width: 0;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 4px 9px;
    border-radius: 5px;
    color: var(--muted);
    cursor: pointer;
    transition:
      color 0.12s ease,
      background-color 0.12s ease;
  }

  .wt.hl,
  .wt[aria-checked="true"] {
    color: var(--fg);
  }

  .wt.hl {
    background: var(--row-hover);
  }

  .wt svg {
    flex: none;
  }

  .wtlabel {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .wtswitch {
    flex: none;
    position: relative;
    width: 24px;
    height: 14px;
    border: 1px solid var(--edge);
    border-radius: 8px;
    background: var(--row-hover);
    transition:
      background-color 0.14s ease,
      border-color 0.14s ease;
  }

  .wtknob {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--muted);
    transition:
      transform 0.14s ease,
      background-color 0.14s ease;
  }

  .wtswitch.on {
    background: color-mix(in srgb, var(--accent) 28%, var(--row-hover));
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
  }

  .wtswitch.on .wtknob {
    transform: translateX(10px);
    background: var(--accent);
  }

  @media (prefers-reduced-motion: reduce) {
    .wtswitch,
    .wtknob {
      transition: none;
    }
  }

  .foot {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-top: 5px;
    padding: 5px 0 0;
    border-top: 1px solid var(--edge);
    font-size: var(--text-xs);
    color: var(--muted);
    user-select: none;
  }

  .foot .hint {
    padding-left: 9px;
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
    padding: 4px 9px;
    border-radius: 5px;
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
