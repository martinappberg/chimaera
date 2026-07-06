<script lang="ts">
  import { onMount } from "svelte";
  import { getActiveWorkspaceId, pollHealth, setActiveWorkspaceId, type Health } from "./lib/api";
  import {
    createSession,
    deleteSession,
    listWorkspaces,
    pollSessions,
    type Session,
    type Workspace,
  } from "./lib/sessions";
  import { reconnectingSockets } from "./lib/ws";
  import FolderPicker from "./lib/FolderPicker.svelte";
  import TerminalView from "./lib/Terminal.svelte";

  let health = $state<Health | null>(null);
  let daemonOk = $state(false);
  let workspaces = $state<Workspace[]>([]);
  let sessions = $state<Session[]>([]);
  let activeId = $state<string | null>(null);
  let activeWsId = $state<string | null>(getActiveWorkspaceId());
  let pickerOpen = $state(false);

  const workspace = $derived(workspaces.find((w) => w.id === activeWsId) ?? null);
  const wsSessions = $derived(sessions.filter((s) => s.workspace_id === activeWsId));
  const sessionIds = $derived(wsSessions.map((s) => s.id));

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

  $effect(() =>
    pollSessions(applySessions, () => {
      // transient poll failure; the daemon dot already reflects reachability
    }),
  );

  onMount(() => {
    refreshWorkspaces();

    const onKeydown = (e: KeyboardEvent) => {
      // The only chords intercepted, even when the terminal has focus:
      // Cmd/Ctrl+O opens the folder picker, Cmd/Ctrl+1..9 switch sessions.
      if ((e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey) {
        if (e.key === "o" || e.key === "O") {
          e.preventDefault();
          e.stopPropagation();
          openPicker();
          return;
        }
        if (pickerOpen) return;
        const n = Number.parseInt(e.key, 10);
        if (n >= 1 && n <= 9 && n <= wsSessions.length) {
          e.preventDefault();
          e.stopPropagation();
          activeId = wsSessions[n - 1].id;
        }
      }
    };
    window.addEventListener("keydown", onKeydown, true);
    return () => window.removeEventListener("keydown", onKeydown, true);
  });

  $effect(() => {
    document.title = workspace ? `${workspace.name} — chimaera` : "chimaera";
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
          ensureActive();
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
    activeWsId = w.id;
    setActiveWorkspaceId(w.id);
    pickerOpen = false;
    ensureActive();
  }

  /** Keep activeId pointing at a session of the active workspace. */
  function ensureActive(): void {
    if (activeId === null || !wsSessions.some((s) => s.id === activeId)) {
      activeId = wsSessions[0]?.id ?? null;
    }
  }

  function applySessions(list: Session[]): void {
    list.sort((a, b) => a.created_at - b.created_at || a.id.localeCompare(b.id));
    sessions = list;
    ensureActive();
  }

  function onTitle(id: string, title: string): void {
    const s = sessions.find((x) => x.id === id);
    if (s) s.title = title;
  }

  function onExited(id: string, status: number | null): void {
    const s = sessions.find((x) => x.id === id);
    if (s) {
      s.alive = false;
      s.exit_status = status;
    }
  }

  async function newTerminal(): Promise<void> {
    if (activeWsId === null) {
      openPicker();
      return;
    }
    try {
      const s = await createSession(activeWsId);
      sessions.push(s);
      activeId = s.id;
    } catch {
      // creation failed; the next poll keeps the list truthful
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
</script>

<div class="shell">
  <aside class="rail">
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
    </div>

    <nav class="sessions">
      {#each wsSessions as s (s.id)}
        <div
          class="row"
          class:active={s.id === activeId}
          role="button"
          tabindex="0"
          onclick={() => (activeId = s.id)}
          onkeydown={(e) => {
            if (e.key === "Enter" || e.key === " ") {
              e.preventDefault();
              activeId = s.id;
            }
          }}
        >
          <span class="dot" class:alive={s.alive}></span>
          <span class="labels">
            <span class="name">{s.name}</span>
            {#if s.title && s.title !== s.name}
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
      <button class="row new" onclick={() => void newTerminal()}>+ new terminal</button>
    </nav>

    <div class="daemon">
      <span
        class="daemon-dot"
        class:ok={daemonOk}
        class:pulse={$reconnectingSockets > 0}
        role="status"
        aria-label={daemonOk ? "connected" : "disconnected"}
      ></span>
      <span class="daemon-host">{health?.hostname ?? "—"}</span>
    </div>
  </aside>

  <main class="stage">
    {#if activeWsId === null}
      <div class="empty">
        <button class="open-cta" onclick={openPicker}>Open a folder</button>
      </div>
    {:else if wsSessions.length === 0}
      <div class="empty">No sessions — create a terminal</div>
    {:else}
      <TerminalView {activeId} {sessionIds} {onTitle} {onExited} />
    {/if}
  </main>
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
    height: 100vh;
    overflow: hidden;
  }

  .rail {
    width: 230px;
    flex: none;
    display: flex;
    flex-direction: column;
    background: var(--rail-bg);
    padding: 0.9rem 0 0.65rem;
  }

  .workspace {
    padding: 0 0.9rem 0.9rem;
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
    max-width: 100%;
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

  .dot {
    flex: none;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--muted);
    opacity: 0.55;
  }

  .dot.alive {
    background: var(--accent);
    opacity: 1;
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
    position: relative;
    min-width: 0;
    background: var(--bg);
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
</style>
