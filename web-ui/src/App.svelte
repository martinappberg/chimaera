<script lang="ts">
  import { onMount } from "svelte";
  import {
    ApiError,
    getActiveWorkspaceId,
    getHostLabel,
    pollHealth,
    setActiveWorkspaceId,
    type Health,
  } from "./lib/api";
  import {
    createSession,
    deleteSession,
    dotState,
    listWorkspaces,
    needsAttention,
    pollSessions,
    type Session,
    type SessionKind,
    type Workspace,
  } from "./lib/sessions";
  import { EventsSocket } from "./lib/events";
  import { reconnectingSockets } from "./lib/ws";
  import {
    activateTab,
    allSessionIds,
    cycleTab,
    defaultLayout,
    deserializeLayout,
    detachTab,
    dropSession,
    findPane,
    focusPane,
    focusedSession as focusedSessionOf,
    moveFocus,
    moveTabToIndex,
    openSession,
    pruneSessions,
    serializeLayout,
    setRatio,
    splitPane,
    toggleZoom,
    type FocusDir,
    type Layout,
    type SplitDir,
  } from "./lib/layout";
  import {
    paneContentEl,
    startDrag,
    type DropSpot,
    type LayoutCtrl,
  } from "./lib/dnd";
  import * as pool from "./lib/termPool";
  import { flushViewState, loadViewState, saveViewState, windowKey } from "./lib/viewState";
  import FolderPicker from "./lib/FolderPicker.svelte";
  import SplitTree from "./lib/SplitNode.svelte";
  import Pane from "./lib/Pane.svelte";

  let health = $state<Health | null>(null);
  let daemonOk = $state(false);
  let workspaces = $state<Workspace[]>([]);
  let sessions = $state<Session[]>([]);
  let activeWsId = $state<string | null>(getActiveWorkspaceId());
  let pickerOpen = $state(false);
  let eventsUp = $state(false);
  let agentError = $state<string | null>(null);

  // In-window layout: the tree is daemon-owned per-window view state; until
  // the GET resolves the stage stays blank (fast, local) so a restored tree
  // never flashes through the single-pane default.
  let layout = $state<Layout>(defaultLayout());
  let layoutReady = $state(false);
  let gotSessions = $state(false);
  let autoOpened = false;
  let dropSpot = $state<DropSpot | null>(null);

  const winKey = windowKey();

  const workspace = $derived(workspaces.find((w) => w.id === activeWsId) ?? null);
  const wsSessions = $derived(sessions.filter((s) => s.workspace_id === activeWsId));
  const sessionsById = $derived(new Map(sessions.map((s) => [s.id, s])));
  const focusedSessionId = $derived(focusedSessionOf(layout));
  const zoomedPane = $derived(
    layout.zoomedPaneId !== null ? findPane(layout.root, layout.zoomedPaneId) : null,
  );

  /** Sessions in the active workspace waiting on the user. */
  const needsYou = $derived(wsSessions.filter(needsAttention).length);

  // Row name is the agent's own title when it has one; duplicate display
  // names within a workspace get a " · n" suffix.
  const displayNames = $derived.by(() => {
    const counts = new Map<string, number>();
    const names = new Map<string, string>();
    for (const s of wsSessions) {
      const base = s.agent_title ?? s.name;
      const n = (counts.get(base) ?? 0) + 1;
      counts.set(base, n);
      names.set(s.id, n === 1 ? base : `${base} · ${n}`);
    }
    return names;
  });

  $effect(() =>
    pollHealth(
      (h) => {
        health = h;
        daemonOk = true;
      },
      () => {
        daemonOk = false;
      },
    ),
  );

  // /ws/events pushes full session snapshots; the 5s poll only runs as a
  // fallback while the socket is down (including before the first frame).
  $effect(() => {
    if (eventsUp) return;
    return pollSessions(applySessions, () => {
      // transient poll failure; the daemon dot already reflects reachability
    });
  });

  // Persist the layout (debounced in viewState) whenever it changes.
  $effect(() => {
    const blob = { v: 1, ws: activeWsId, layout: serializeLayout(layout) };
    if (!layoutReady) return;
    saveViewState(winKey, blob);
  });

  // Dispose pooled terminals for sessions that no longer exist.
  $effect(() => {
    const ids = sessions.map((s) => s.id);
    if (!gotSessions) return;
    pool.syncSessions(ids);
  });

  onMount(() => {
    pool.initPool({ onTitle, onExited });
    const events = new EventsSocket({
      onSessions: applySessions,
      onStatus: (up) => (eventsUp = up),
    });
    refreshWorkspaces();
    void bootViewState();

    const onPagehide = () => void flushViewState();
    window.addEventListener("keydown", onKeydown, true);
    window.addEventListener("pagehide", onPagehide);
    return () => {
      window.removeEventListener("keydown", onKeydown, true);
      window.removeEventListener("pagehide", onPagehide);
      events.close();
      pool.disposePool();
    };
  });

  /** Restore this window's layout from the daemon; anything missing/invalid
   *  (including a not-yet-upgraded daemon) falls back to the default. */
  async function bootViewState(): Promise<void> {
    const raw = await loadViewState(winKey);
    if (
      typeof raw === "object" &&
      raw !== null &&
      (raw as { v?: unknown }).v === 1 &&
      (raw as { ws?: unknown }).ws === activeWsId
    ) {
      const restored = deserializeLayout((raw as { layout?: unknown }).layout);
      if (restored !== null) layout = restored;
    }
    layoutReady = true;
    pruneAndAutoOpen();
  }

  /**
   * The chords intercepted even when a terminal has focus (capture phase;
   * everything is modifier-gated so plain keys always reach the PTY):
   *   mod+O picker · mod+1..9 open Nth session · mod+D / mod+Shift+D splits
   *   mod+Alt+arrows focus · mod+Alt+[ ] tabs · mod+Shift+Enter zoom · mod+B
   *   focus mode. Cmd+W/Cmd+T stay unbound (browser collision).
   */
  function onKeydown(e: KeyboardEvent): void {
    const mod = e.metaKey || e.ctrlKey;
    if (!mod) return;
    const key = e.key.length === 1 ? e.key.toLowerCase() : e.key;
    const intercept = () => {
      e.preventDefault();
      e.stopPropagation();
    };
    if (!e.altKey && !e.shiftKey && key === "o") {
      intercept();
      openPicker();
      return;
    }
    if (pickerOpen) return;
    if (activeWsId === null || !layoutReady) return;

    if (e.altKey && !e.shiftKey) {
      const dirs: Record<string, FocusDir> = {
        ArrowLeft: "left",
        ArrowRight: "right",
        ArrowUp: "up",
        ArrowDown: "down",
      };
      const dir = dirs[e.key];
      if (dir !== undefined) {
        intercept();
        focusDirection(dir);
      } else if (e.code === "BracketLeft") {
        intercept();
        cycle(-1);
      } else if (e.code === "BracketRight") {
        intercept();
        cycle(1);
      }
      return;
    }
    if (e.shiftKey && !e.altKey) {
      if (key === "d") {
        intercept();
        split("col");
      } else if (key === "Enter") {
        intercept();
        layout = toggleZoom(layout);
      }
      return;
    }
    if (!e.shiftKey && !e.altKey) {
      if (key === "d") {
        intercept();
        split("row");
        return;
      }
      if (key === "b") {
        intercept();
        layout = { ...layout, focusMode: !layout.focusMode };
        return;
      }
      const n = Number.parseInt(key, 10);
      if (n >= 1 && n <= 9 && n <= wsSessions.length) {
        intercept();
        openSess(wsSessions[n - 1].id);
      }
    }
  }

  $effect(() => {
    const base = workspace ? `${workspace.name} — chimaera` : "chimaera";
    document.title = needsYou > 0 ? `(${needsYou}) ${base}` : base;
  });

  /**
   * Refresh the workspace list; if the tab's stored workspace no longer
   * exists on the daemon, clear it and fall back to the empty state.
   */
  function refreshWorkspaces(): void {
    void listWorkspaces()
      .then((list) => {
        workspaces = list;
        if (activeWsId !== null && !list.some((w) => w.id === activeWsId)) {
          activeWsId = null;
          setActiveWorkspaceId(null);
        }
      })
      .catch(() => {
        // daemon unreachable; health polling surfaces this
      });
  }

  function openPicker(): void {
    refreshWorkspaces();
    pickerOpen = true;
  }

  /** Scope this window to `w` (open in THIS window). */
  function activateWorkspace(w: Workspace): void {
    workspaces = workspaces.some((x) => x.id === w.id)
      ? workspaces.map((x) => (x.id === w.id ? w : x))
      : [w, ...workspaces];
    const switched = activeWsId !== w.id;
    activeWsId = w.id;
    setActiveWorkspaceId(w.id);
    pickerOpen = false;
    agentError = null;
    if (switched) {
      // The layout tree is per workspace window; a workspace switch starts
      // clean and auto-opens the new workspace's first session.
      layout = defaultLayout();
      autoOpened = false;
      pruneAndAutoOpen();
    }
  }

  function applySessions(list: Session[]): void {
    list.sort((a, b) => a.created_at - b.created_at || a.id.localeCompare(b.id));
    sessions = list;
    gotSessions = true;
    pruneAndAutoOpen();
  }

  /**
   * Once both the persisted layout and the first session snapshot are in:
   * drop tabs whose sessions vanished (also on every later snapshot), and —
   * exactly once — populate a pristine layout with the first session.
   */
  function pruneAndAutoOpen(): void {
    if (!layoutReady || !gotSessions) return;
    layout = pruneSessions(layout, new Set(sessions.map((s) => s.id)));
    if (!autoOpened) {
      autoOpened = true;
      if (allSessionIds(layout).length === 0 && wsSessions.length > 0) {
        layout = openSession(layout, wsSessions[0].id);
      }
    }
  }

  function onTitle(id: string, title: string): void {
    const s = sessions.find((x) => x.id === id);
    if (s) s.title = title;
  }

  function onExited(id: string, _status: number | null): void {
    // Exited sessions vanish, tmux-style — the daemon has already reaped
    // them; drop the row without waiting for the next poll.
    applySessions(sessions.filter((s) => s.id !== id));
  }

  /** Open/focus a session in the layout (rail click, strip chip, mod+N). */
  function openSess(id: string): void {
    layout = openSession(layout, id);
    pool.focusTerminal(id);
  }

  function focusDirection(dir: FocusDir): void {
    layout = moveFocus(layout, dir);
    const sid = focusedSessionOf(layout);
    if (sid !== null) pool.focusTerminal(sid);
  }

  function cycle(delta: number): void {
    layout = cycleTab(layout, delta);
    const sid = focusedSessionOf(layout);
    if (sid !== null) pool.focusTerminal(sid);
  }

  function split(dir: SplitDir): void {
    layout = splitPane(layout, layout.focusedPaneId, dir);
    // The new pane is empty: pull DOM focus off the old terminal so typing
    // doesn't land in a pane that no longer looks focused.
    if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
  }

  /** The focused pane's fitted size, for spawning sessions at the right grid. */
  function spawnSize(): { cols: number; rows: number } | null {
    const pane = findPane(layout.root, layout.focusedPaneId);
    if (pane === null) return null;
    const sid = pane.tabs[pane.active]?.sessionId;
    if (sid !== undefined) {
      const exact = pool.getSize(sid);
      if (exact !== null) return exact;
    }
    const el = paneContentEl(pane.id);
    return el !== null ? pool.estimateSize(el) : null;
  }

  async function newSession(kind: SessionKind): Promise<void> {
    if (activeWsId === null) {
      openPicker();
      return;
    }
    agentError = null;
    try {
      const s = await createSession(activeWsId, kind, null, spawnSize());
      sessions.push(s);
      openSess(s.id);
    } catch (e) {
      // Shell failures stay quiet (the next snapshot keeps the list
      // truthful); agent failures carry an actionable message (409 when
      // claude is not installed) worth a line under the button.
      if (kind === "agent") {
        agentError = e instanceof ApiError ? e.message : "failed to start agent";
      }
    }
  }

  async function closeSession(id: string): Promise<void> {
    try {
      await deleteSession(id);
    } catch {
      // already gone or unreachable; fall through and drop it locally
    }
    applySessions(sessions.filter((s) => s.id !== id));
  }

  // --- layout controller (invoked by the pane tree) -------------------------

  const ctrl: LayoutCtrl = {
    focusPane(paneId) {
      layout = focusPane(layout, paneId);
    },
    activateTab(paneId, index) {
      layout = activateTab(layout, paneId, index);
      const sid = focusedSessionOf(layout);
      if (sid !== null) pool.focusTerminal(sid);
    },
    closeTab(paneId, index) {
      // Detaches the view only — the session stays alive in the rail.
      layout = detachTab(layout, paneId, index);
    },
    setRatio(splitId, ratio) {
      layout = setRatio(layout, splitId, ratio);
    },
    dragTab(e, paneId, index, sessionId) {
      beginDrag(e, sessionId, () => ctrl.activateTab(paneId, index));
    },
    dividerDrag(active) {
      pool.setDragging(active);
    },
  };

  /** Shared drag start for rail rows and pane tabs. */
  function beginDrag(e: PointerEvent, sessionId: string, onClick: () => void): void {
    const label =
      displayNames.get(sessionId) ?? sessionsById.get(sessionId)?.name ?? sessionId.slice(0, 8);
    startDrag(
      e,
      { sessionId, label },
      {
        onSpot: (s) => (dropSpot = s),
        onDrop: (spot) => {
          layout =
            spot.kind === "tab"
              ? moveTabToIndex(layout, sessionId, spot.paneId, spot.index)
              : dropSession(layout, sessionId, spot.paneId, spot.zone);
          pool.focusTerminal(sessionId);
        },
        onClick,
        onEnd: () => (dropSpot = null),
      },
    );
  }

  function onRailRowDown(e: PointerEvent, sessionId: string): void {
    beginDrag(e, sessionId, () => openSess(sessionId));
  }
</script>

<div class="shell">
  <div class="body">
    <aside class="rail" class:collapsed={layout.focusMode}>
      <div class="workspace">
        <button
          class="ws-btn"
          class:placeholder={workspace === null && activeWsId === null}
          title={workspace?.root}
          onclick={openPicker}
        >
          <span class="ws-label">
            {workspace ? workspace.name : activeWsId !== null ? "—" : "Open a folder"}
          </span>
          <svg class="ws-chev" viewBox="0 0 16 16" width="10" height="10" aria-hidden="true">
            <path
              d="M4 6l4 4 4-4"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
              stroke-linejoin="round"
            />
          </svg>
        </button>
        {#if needsYou > 0}
          <span class="needs" title="{needsYou} need{needsYou === 1 ? 's' : ''} you">
            {needsYou}
          </span>
        {/if}
      </div>

      <nav class="sessions">
        {#each wsSessions as s (s.id)}
          <div
            class="row"
            class:active={s.id === focusedSessionId}
            role="button"
            tabindex="0"
            onpointerdowncapture={(e) => {
              // Capture-phase (directly attached); the close button stays a
              // plain click.
              if (e.target instanceof Element && e.target.closest(".close")) return;
              onRailRowDown(e, s.id);
            }}
            onkeydown={(e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                openSess(s.id);
              }
            }}
          >
            <span class="dot {dotState(s)}"></span>
            <span class="labels">
              <span class="name">{displayNames.get(s.id) ?? s.name}</span>
              {#if s.title && s.title !== s.name && s.title !== s.agent_title}
                <span class="title">{s.title}</span>
              {/if}
            </span>
            <button
              class="close"
              aria-label="close session"
              title="close"
              onclick={(e) => {
                e.stopPropagation();
                void closeSession(s.id);
              }}>&times;</button
            >
          </div>
        {/each}
        <button class="row new primary" onclick={() => void newSession("agent")}>+ new agent</button>
        {#if agentError}
          <div class="agent-error">{agentError}</div>
        {/if}
        <button class="row new" onclick={() => void newSession("shell")}>+ terminal</button>
      </nav>

      <div class="daemon">
        <span
          class="daemon-dot"
          class:ok={daemonOk}
          class:pulse={$reconnectingSockets > 0}
          role="status"
          aria-label={daemonOk ? "connected" : "disconnected"}
        ></span>
        <span class="daemon-host" title={health?.hostname}>{getHostLabel()}</span>
      </div>
    </aside>

    <main class="stage">
      {#if activeWsId === null}
        <div class="empty">
          <button class="open-cta" onclick={openPicker}>Open a folder</button>
        </div>
      {:else if layoutReady}
        {#if zoomedPane !== null}
          <Pane
            node={zoomedPane}
            focusedPaneId={layout.focusedPaneId}
            zoomed
            {dropSpot}
            sessions={sessionsById}
            names={displayNames}
            {ctrl}
          />
        {:else}
          <SplitTree
            node={layout.root}
            focusedPaneId={layout.focusedPaneId}
            {dropSpot}
            sessions={sessionsById}
            names={displayNames}
            {ctrl}
          />
        {/if}
      {/if}
    </main>
  </div>

  {#if layout.focusMode}
    <!-- Focus-mode session strip: the rail is gone, but the window always
         says where you are. Hidden whenever the rail is visible. -->
    <footer class="strip">
      <span class="strip-ws" title={workspace?.root}>{workspace?.name ?? "chimaera"}</span>
      <div class="chips">
        {#each wsSessions as s (s.id)}
          <button
            class="chip"
            class:focused={s.id === focusedSessionId}
            title={s.title ?? undefined}
            onclick={() => openSess(s.id)}
          >
            <span class="dot {dotState(s)}"></span>
            <span class="chip-name">{s.kind === "shell" ? "$ " : ""}{displayNames.get(s.id) ?? s.name}</span>
          </button>
        {/each}
      </div>
      {#if needsYou > 0}
        <span class="strip-needs">{needsYou} need{needsYou === 1 ? "s" : ""} you</span>
      {/if}
      <span class="strip-host" title={health?.hostname}>{getHostLabel()}</span>
    </footer>
  {/if}
</div>

{#if pickerOpen}
  <FolderPicker
    recents={workspaces}
    onOpened={activateWorkspace}
    onClose={() => (pickerOpen = false)}
  />
{/if}

<style>
  .shell {
    display: flex;
    flex-direction: column;
    height: 100vh;
    overflow: hidden;
  }

  .body {
    flex: 1;
    display: flex;
    min-height: 0;
  }

  .rail {
    width: 230px;
    flex: none;
    display: flex;
    flex-direction: column;
    background: var(--rail-bg);
    padding: 0.9rem 0 0.65rem;
    overflow: hidden;
    transition:
      width 0.12s ease,
      opacity 0.1s ease;
  }

  /* Focus mode: the rail collapses to nothing; the strip carries context. */
  .rail.collapsed {
    width: 0;
    padding-left: 0;
    padding-right: 0;
    opacity: 0;
    visibility: hidden;
  }

  .workspace {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0 0.9rem 0.9rem;
  }

  .needs {
    flex: none;
    font-size: 0.72rem;
    font-variant-numeric: tabular-nums;
    color: var(--warn);
  }

  .ws-btn {
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    font-size: 0.85rem;
    font-weight: 600;
    letter-spacing: 0.01em;
    color: var(--fg);
    cursor: pointer;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 0.3rem;
  }

  .ws-btn.placeholder {
    font-weight: 400;
    color: var(--muted);
  }

  .ws-btn.placeholder:hover {
    color: var(--fg);
  }

  .ws-label {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .ws-chev {
    flex: none;
    color: var(--muted);
    opacity: 0.7;
  }

  .sessions {
    flex: 1;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 1px;
    padding: 0 0.45rem;
    min-height: 0;
  }

  .row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.35rem 0.45rem;
    border-radius: 5px;
    font-size: 0.85rem;
    cursor: pointer;
    user-select: none;
  }

  .row:hover {
    background: var(--row-hover);
  }

  .row.active {
    background: var(--row-active);
  }

  .labels {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    line-height: 1.3;
  }

  .name,
  .title {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .name {
    font-family: var(--mono);
    font-size: 0.78rem;
  }

  .title {
    font-size: 0.72rem;
    color: var(--muted);
  }

  .close {
    appearance: none;
    border: none;
    background: none;
    padding: 0 0.1rem;
    font: inherit;
    font-size: 0.9rem;
    line-height: 1;
    color: var(--muted);
    cursor: pointer;
    opacity: 0;
    flex: none;
  }

  .row:hover .close,
  .row:focus-within .close {
    opacity: 1;
  }

  .close:hover {
    color: var(--fg);
  }

  .row.new {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: 0.82rem;
    color: var(--muted);
    justify-content: flex-start;
    margin-top: 0.15rem;
  }

  .row.new:hover {
    background: var(--row-hover);
    color: var(--fg);
  }

  .row.new.primary {
    color: var(--fg);
    font-weight: 500;
  }

  .agent-error {
    padding: 0.1rem 0.45rem 0.25rem;
    font-size: 0.72rem;
    line-height: 1.35;
    color: var(--err);
  }

  .daemon {
    display: flex;
    align-items: center;
    gap: 0.45rem;
    padding: 0.65rem 0.9rem 0;
    font-size: 0.72rem;
    color: var(--muted);
  }

  .daemon-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--muted);
    opacity: 0.55;
    transition: background-color 0.3s ease;
  }

  .daemon-dot.ok {
    background: var(--accent);
    opacity: 1;
  }

  .daemon-dot.pulse {
    animation: pulse 1.2s ease-in-out infinite;
  }

  @keyframes pulse {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0.25;
    }
  }

  .daemon-host {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .stage {
    flex: 1;
    display: flex;
    min-width: 0;
    min-height: 0;
    position: relative;
    background: var(--bg);
    padding: 10px;
  }

  .empty {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--muted);
    font-size: 0.9rem;
  }

  .open-cta {
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    font-size: 0.9rem;
    color: var(--muted);
    cursor: pointer;
  }

  .open-cta:hover {
    color: var(--fg);
  }

  /* --- session strip (focus mode) --- */

  .strip {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.7rem;
    height: 34px;
    padding: 0 0.9rem;
    background: var(--rail-bg);
    border-top: 1px solid var(--edge);
    font-size: 0.72rem;
    color: var(--muted);
  }

  .strip-ws {
    flex: none;
    font-weight: 600;
    color: var(--fg);
    max-width: 180px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .chips {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 0.25rem;
    overflow-x: auto;
    scrollbar-width: none;
  }

  .chips::-webkit-scrollbar {
    display: none;
  }

  .chip {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.4rem;
    appearance: none;
    border: none;
    background: none;
    padding: 0.15rem 0.55rem;
    border-radius: 4px;
    font-family: var(--mono);
    font-size: 0.72rem;
    color: var(--muted);
    cursor: pointer;
    max-width: 180px;
  }

  .chip:hover {
    background: var(--row-hover);
    color: var(--fg);
  }

  /* The focused session's chip is inverted — findable at a glance. */
  .chip.focused {
    background: var(--fg);
    color: var(--bg);
  }

  .chip.focused:hover {
    background: var(--fg);
    color: var(--bg);
  }

  .chip-name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .strip-needs {
    flex: none;
    color: var(--warn);
    font-variant-numeric: tabular-nums;
  }

  .strip-host {
    flex: none;
    max-width: 140px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
