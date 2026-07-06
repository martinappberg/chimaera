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
    listWorkspaces,
    needsAttention,
    pollSessions,
    type Session,
    type SessionKind,
    type Workspace,
  } from "./lib/sessions";
  import { EventsSocket } from "./lib/events";
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
  let eventsUp = $state(false);
  let agentError = $state<string | null>(null);

  const workspace = $derived(workspaces.find((w) => w.id === activeWsId) ?? null);
  const wsSessions = $derived(sessions.filter((s) => s.workspace_id === activeWsId));
  const sessionIds = $derived(wsSessions.map((s) => s.id));

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

  /** Dot modifier class for a session row (see .dot.* styles). */
  function dotState(s: Session): string {
    if (s.kind !== "agent") return s.alive ? "alive" : "";
    switch (s.agent_state) {
      case "running":
        return "alive";
      case "needs_permission":
      case "idle_prompt":
        return "attn";
      case "finished":
        return "done";
      case "errored":
        return "err";
      case "rate_limited":
        return "rate";
      default:
        return "";
    }
  }

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

  onMount(() => {
    const events = new EventsSocket({
      onSessions: applySessions,
      onStatus: (up) => (eventsUp = up),
    });
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
    return () => {
      window.removeEventListener("keydown", onKeydown, true);
      events.close();
    };
  });

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
    agentError = null;
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

  function onExited(id: string, _status: number | null): void {
    // Exited sessions vanish, tmux-style — the daemon has already reaped
    // them; drop the row without waiting for the next poll.
    applySessions(sessions.filter((s) => s.id !== id));
  }

  async function newSession(kind: SessionKind): Promise<void> {
    if (activeWsId === null) {
      openPicker();
      return;
    }
    agentError = null;
    try {
      const s = await createSession(activeWsId, kind);
      sessions.push(s);
      activeId = s.id;
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
    {:else if wsSessions.length === 0}
      <div class="empty">No sessions — start an agent</div>
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

  .dot {
    flex: none;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--muted);
    opacity: 0.55;
  }

  /* Agent attention states; the base .dot covers gone/unknown. */
  .dot.alive {
    background: var(--accent);
    opacity: 1;
  }

  .dot.attn {
    background: var(--warn);
    opacity: 1;
  }

  .dot.err {
    background: var(--err);
    opacity: 1;
  }

  .dot.rate {
    background: var(--rate);
    opacity: 1;
  }

  .dot.done {
    opacity: 0.85;
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
