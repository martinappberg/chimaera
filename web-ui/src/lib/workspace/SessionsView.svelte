<script lang="ts">
  /**
   * All sessions: every past session of the workspace, from its history
   * records (`GET /workspaces/{id}/history`), grouped by day. A row is the
   * agent glyph and the title (else the first prompt), then — muted, right —
   * who started it when it wasn't you, duration, files and tokens. Clicking
   * opens the conversation (a running one, or a resume through the Recents
   * flow while the agent still has it; "Resume" shows on hover). A row whose
   * conversation is gone is muted and says so. The files count opens what
   * the session changed. Search matches titles and first prompts; the agent
   * filter shows only when more than one agent kind exists, and "Archived"
   * (conversations hidden from Recents, with Unarchive) only when there are
   * some; the Mastermind's actions are a quiet link away. Pulled while
   * visible and on the recents nudge (a session ended, a row archived) —
   * never polled.
   */
  import { untrack } from "svelte";
  import type { DashCtx } from "../dashboard/dash";
  import type { LayoutCtrl } from "../layout/dnd";
  import { ApiError } from "../net/api";
  import SessionGlyph from "../shared/SessionGlyph.svelte";
  import { contextMenu } from "../shared/contextMenu.svelte";
  import { dotState, type Session } from "./sessions";
  import {
    commitCount,
    fetchArchived,
    fetchHistory,
    formatDuration,
    formatTokens,
    historyNudge,
    recordTitle,
    startedByLabel,
    unarchiveRecents,
    type ArchivedConvo,
    type HistoryAct,
    type HistoryRecord,
  } from "./history";
  import SessionEdits from "./SessionEdits.svelte";
  import { openGitView } from "./git";

  interface Props {
    dash: DashCtx;
    sessions: Map<string, Session>;
    names: Map<string, string>;
    wsId: string | null;
    wsRoot: string | null;
    paneId: string;
    ctrl: LayoutCtrl;
    /** False while this retained tab is behind another pane tab. */
    visible?: boolean;
  }

  let { dash, sessions, names, wsId, wsRoot, paneId, ctrl, visible = true }: Props = $props();

  const PAGE = 50;

  let records = $state<HistoryRecord[]>([]);
  let acts = $state<HistoryAct[]>([]);
  let more = $state(false);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let loaded = $state(false);
  /** Agent kinds this workspace has shown (the filter appears past one). */
  let kinds = $state<string[]>([]);

  let query = $state("");
  /** The query the list was fetched with (debounced). */
  let applied = $state("");
  let agent = $state("");
  let showActs = $state(false);
  let expanded = $state<string | null>(null);
  /** Conversations archived out of Recents (the "Archived" filter). */
  let archived = $state<ArchivedConvo[]>([]);
  let showArchived = $state(false);

  // Typing settles for a beat before a fetch.
  $effect(() => {
    const q = query;
    const t = setTimeout(() => (applied = q), 250);
    return () => clearTimeout(t);
  });

  // A new workspace starts its filter afresh.
  $effect(() => {
    void wsId;
    untrack(() => {
      kinds = [];
      agent = "";
      showActs = false;
      showArchived = false;
      archived = [];
      expanded = null;
    });
  });

  /** Monotone fetch counter: a slow early page must not clobber a newer one. */
  let seq = 0;

  async function load(reset: boolean): Promise<void> {
    const ws = wsId;
    if (ws === null) return;
    const mine = ++seq;
    loading = true;
    try {
      const before = reset ? undefined : records.at(-1)?.started;
      const [page, arch] = await Promise.all([
        fetchHistory(ws, { before, q: applied, agent, limit: PAGE, acts: reset }),
        reset ? fetchArchived(ws).catch(() => null) : Promise.resolve(null),
      ]);
      if (mine !== seq) return;
      if (arch !== null) {
        archived = arch;
        if (arch.length === 0) showArchived = false;
      }
      records = reset ? page.records : [...records, ...page.records];
      more = page.more;
      if (reset) acts = page.acts ?? [];
      const seen = new Set(kinds);
      for (const r of page.records) seen.add(r.agent);
      if (seen.size !== kinds.length) kinds = [...seen].sort();
      error = null;
      loaded = true;
    } catch (e) {
      if (mine !== seq) return;
      error =
        e instanceof ApiError && e.status === 404
          ? "This daemon has no session history yet — update chimaera to see every past session here."
          : e instanceof Error
            ? e.message
            : String(e);
    } finally {
      if (mine === seq) loading = false;
    }
  }

  // Fetch while visible: on show, on a new search/filter, and on the
  // recents nudge (a session ended somewhere in this workspace).
  $effect(() => {
    if (!visible || wsId === null) return;
    void applied;
    void agent;
    void $historyNudge;
    untrack(() => void load(true));
  });

  /** Day groups, newest first: Today, Yesterday, Sep 26. */
  const days = $derived.by(() => {
    const out: { key: string; label: string; rows: HistoryRecord[] }[] = [];
    const today = new Date();
    const yesterday = new Date(today.getTime() - 86_400_000);
    const same = (a: Date, b: Date) =>
      a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();
    for (const r of records) {
      const d = new Date(r.started);
      const key = `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`;
      const label = same(d, today)
        ? "Today"
        : same(d, yesterday)
          ? "Yesterday"
          : d.toLocaleDateString([], {
              month: "short",
              day: "numeric",
              year: d.getFullYear() === today.getFullYear() ? undefined : "numeric",
            });
      const last = out.at(-1);
      if (last !== undefined && last.key === key) last.rows.push(r);
      else out.push({ key, label, rows: [r] });
    }
    return out;
  });

  /** A name for a session id: live names first, then what the records say. */
  function nameOf(id: string): string | undefined {
    if (id === dash.mastermind?.session_id) return "the Mastermind";
    const live = names.get(id) ?? sessions.get(id)?.display_name ?? undefined;
    if (live) return live;
    const rec = records.find((r) => r.id === id);
    return rec !== undefined ? recordTitle(rec) : undefined;
  }

  /** Who started it, in words — only when it wasn't you. */
  function by(r: HistoryRecord): string | null {
    switch (r.started_by) {
      case "you":
        return null;
      case "mastermind":
        return "by Mastermind";
      case "restart":
        return "after restart";
      default:
        return startedByLabel(r.started_by, nameOf);
    }
  }

  /** Its conversation can't be reopened (a Mastermind's is never, by design
   *  — not "gone"). */
  const gone = (r: HistoryRecord) =>
    !r.live && r.mastermind !== true && (r.reopen?.resume ?? null) === null;

  function activate(r: HistoryRecord): void {
    if (r.live) {
      dash.onOpenSession(r.id);
      return;
    }
    const resume = r.reopen?.resume ?? null;
    if (resume === null) {
      // Nothing to open: show what it did instead.
      toggle(r);
      return;
    }
    dash.onOpenRecent({
      key: resume,
      kind: r.agent,
      title: recordTitle(r),
      resume,
      lastActive: Math.floor((r.ended ?? r.started) / 1000),
      ui: r.reopen?.ui ?? r.ui,
    });
  }

  function toggle(r: HistoryRecord): void {
    expanded = expanded === r.rid ? null : r.rid;
  }

  /** Record paths are workspace-relative; the edit record's are absolute. */
  function abs(p: string): string {
    return wsRoot !== null && !p.startsWith("/") ? `${wsRoot}/${p}` : p;
  }

  function clock(ms: number): string {
    return new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  }

  const ACT_WORDS: Record<string, string> = {
    spawn_agent: "started",
    spawn_terminal: "opened the terminal",
    message_agent: "messaged",
    interrupt_agent: "stopped the turn of",
    deliver_note: "delivered a message to",
    // Agent communication: a wake (a peer's message, or your Wake), and your
    // hand-over of an inbox.
    wake_agent: "woke",
    deliver_messages: "handed waiting messages to",
    wake_mastermind: "woke",
  };

  /** The Mastermind's acts name it by role, whatever its session is called. */
  const MASTERMIND_ACTS = new Set(["spawn_agent", "spawn_terminal", "message_agent", "interrupt_agent"]);

  /** Unarchive: back in Recents (the recents nudge refetches every view). */
  async function unarchive(a: ArchivedConvo): Promise<void> {
    const ws = wsId;
    if (ws === null) return;
    archived = archived.filter((x) => x.key !== a.key);
    if (archived.length === 0) showArchived = false;
    try {
      await unarchiveRecents(ws, [a.key]);
    } catch {
      void load(true);
    }
  }

  function day(secs: number): string {
    return new Date(secs * 1000).toLocaleDateString([], { month: "short", day: "numeric" });
  }

  function actor(a: HistoryAct): string {
    if (a.by === "you") return "You";
    if (MASTERMIND_ACTS.has(a.act)) return "The Mastermind";
    return nameOf(a.by) ?? a.by;
  }
</script>

<div class="sessions">
  <div class="inner">
    <header class="head">
      <h1>{showActs ? "Mastermind actions" : "All sessions"}</h1>
      {#if !showActs && !showArchived}
        <input class="search" type="search" placeholder="Search" bind:value={query} aria-label="search titles and first prompts" />
      {/if}
    </header>

    {#if !showActs && (kinds.length > 1 || archived.length > 0)}
      <div class="filters">
        {#if kinds.length > 1}
          <div class="seg" role="radiogroup" aria-label="agent">
            <button role="radio" aria-checked={agent === "" && !showArchived} class:on={agent === "" && !showArchived} onclick={() => ((agent = ""), (showArchived = false))}>All</button>
            {#each kinds as k (k)}
              <button role="radio" aria-checked={agent === k && !showArchived} class:on={agent === k && !showArchived} onclick={() => ((agent = k), (showArchived = false))}>{k === "claude" ? "Claude" : k === "codex" ? "Codex" : k}</button>
            {/each}
          </div>
        {/if}
        {#if archived.length > 0}
          <button class="seg-one" class:on={showArchived} aria-pressed={showArchived} onclick={() => (showArchived = !showArchived)}>Archived</button>
        {/if}
      </div>
    {/if}

    {#if showArchived && !showActs}
      <div class="rows">
        {#each archived as a (a.key)}
          <div
            class="row arch"
            role="listitem"
            oncontextmenu={(e) => contextMenu.openAt(e, [{ label: "Unarchive", onSelect: () => void unarchive(a) }])}
          >
            <span class="main static">
              <SessionGlyph kind="agent" agentKind={a.kind} size={11} title={a.kind} />
              <span class="text"><span class="title">{a.title || a.kind}</span></span>
              <button class="hint act-btn" onclick={() => void unarchive(a)}>Unarchive</button>
            </span>
            <span class="meta"><span>archived {day(a.at)}</span></span>
          </div>
        {/each}
      </div>
      <p class="quiet">Archived conversations are hidden from Recents; nothing was deleted.</p>
    {:else if showActs}
      <ol class="acts">
        {#each acts as a, i (`${a.ts}:${i}`)}
          <li class="act">
            <span class="when">{new Date(a.ts).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" })}</span>
            <span class="what">
              {actor(a)}
              {ACT_WORDS[a.act] ?? a.act}
              {#if a.target !== undefined}<button class="who" onclick={() => dash.onOpenSession(a.target ?? "")}>{nameOf(a.target) ?? a.target}</button>{/if}
              {#if a.detail !== undefined}<span class="detail">— {a.detail}</span>{/if}
            </span>
          </li>
        {/each}
      </ol>
      <div class="foot"><button class="link" onclick={() => (showActs = false)}>All sessions</button></div>
    {:else if error !== null && records.length === 0}
      <p class="empty">{error}</p>
    {:else if records.length === 0}
      <p class="empty">
        {#if !loaded}&nbsp;{:else if applied !== "" || agent !== ""}No session matches.{:else}No sessions yet. Each agent session lands here.{/if}
      </p>
    {:else}
      {#each days as day (day.key)}
        <section class="day" aria-label={day.label}>
          <h2 class="lbl">{day.label}</h2>
          <div class="rows">
            {#each day.rows as r (r.rid)}
              {@const isOpen = expanded === r.rid}
              {@const commits = commitCount(r)}
              {@const live = r.live ? sessions.get(r.id) : undefined}
              {@const isGone = gone(r)}
              {@const who = by(r)}
              <div class="rec" class:gone={isGone}>
                <div class="row">
                  <button class="main" onclick={() => activate(r)} title={isGone ? (r.reopen?.gone ?? "") : r.live ? "open this session" : "resume this conversation"}>
                    <SessionGlyph kind="agent" agentKind={r.agent} state={live !== undefined ? dotState(live) : ""} size={11} title={r.agent} />
                    <span class="text">
                      <span class="title">{recordTitle(r)}</span>
                      {#if isGone}<span class="sub">Conversation no longer available</span>{/if}
                    </span>
                    {#if r.live || (r.reopen?.resume ?? null) !== null}<span class="hint">{r.live ? "Open" : "Resume"}</span>{/if}
                  </button>
                  <span class="meta">
                    {#if who !== null}<span>{who}</span>{/if}
                    {#if r.outcome === "crashed"}<span>crashed</span>{/if}
                    <span class="dur">{formatDuration((r.ended ?? Date.now()) - r.started)}</span>
                    {#if r.files.n > 0}
                      <button class="files" aria-expanded={isOpen} title="what this session changed" onclick={() => toggle(r)}>{r.files.n} file{r.files.n === 1 ? "" : "s"}</button>
                    {/if}
                    {#if commits !== null && commits > 0}
                      <button class="files" aria-expanded={isOpen} title="the commits this session made" onclick={() => toggle(r)}>{commits} commit{commits === 1 ? "" : "s"}</button>
                    {/if}
                    {#if r.usage.tokens_in !== null || r.usage.tokens_out !== null}
                      <span class="tok">{formatTokens((r.usage.tokens_in ?? 0) + (r.usage.tokens_out ?? 0))} tokens</span>
                    {/if}
                  </span>
                </div>
                {#if isOpen}
                  <div class="detail-box">
                    <p class="facts">
                      {new Date(r.started).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" })}{r.ended !== undefined ? ` – ${clock(r.ended)}` : ""}{(r.models ?? []).length > 0 ? ` · ${(r.models ?? []).join(", ")}` : ""}{r.usage.turns !== null ? ` · ${r.usage.turns} turn${r.usage.turns === 1 ? "" : "s"}` : ""}{r.usage.tokens_in !== null || r.usage.tokens_out !== null ? ` · ${formatTokens((r.usage.tokens_in ?? 0) + (r.usage.tokens_out ?? 0))} tokens` : ""}
                    </p>
                    {#if (r.git?.commits ?? []).length > 0}
                      {@const repo = r.git?.end?.worktree ?? r.git?.start?.worktree ?? null}
                      <ul class="commits" aria-label="Commits this session made">
                        {#each r.git?.commits ?? [] as c (c.sha)}
                          <li>
                            <button
                              class="commit"
                              title="open this commit"
                              onclick={() => openGitView({ surface: "gitx", view: "commit", repo, sha: c.sha, title: c.subject })}
                            >
                              <span class="csubj">{c.subject || "(no message)"}</span>
                              <span class="csha">{c.sha.slice(0, 7)}</span>
                            </button>
                          </li>
                        {/each}
                      </ul>
                    {/if}
                    {#if wsId !== null}
                      <SessionEdits
                        sessionId={r.id}
                        {wsId}
                        {wsRoot}
                        paths={(r.files.top ?? []).map(abs)}
                        {visible}
                        onOpenFile={(p, e) => ctrl.openFileFrom(paneId, p, e.metaKey || e.ctrlKey)}
                      />
                    {/if}
                  </div>
                {/if}
              </div>
            {/each}
          </div>
        </section>
      {/each}
      <div class="foot">
        {#if more}
          <button class="link" disabled={loading} onclick={() => void load(false)}>{loading ? "loading…" : "Older sessions"}</button>
        {/if}
        {#if acts.length > 0}
          <button class="link" onclick={() => (showActs = true)}>Mastermind actions</button>
        {/if}
        {#if error !== null}<span class="err">{error}</span>{/if}
      </div>
    {/if}
  </div>
</div>

<style>
  .sessions {
    position: absolute;
    inset: 0;
    overflow-y: auto;
    background: var(--bg);
    container-type: inline-size;
  }
  .inner {
    max-width: 880px;
    margin: 0 auto;
    padding: 26px 32px 48px;
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 20px;
    flex-wrap: wrap;
  }
  h1 {
    margin: 0;
    font-size: 22px;
    font-weight: 600;
    letter-spacing: -0.01em;
  }
  .search {
    margin-left: auto;
    width: 220px;
    max-width: 100%;
    padding: 4px 10px;
    border: 1px solid var(--edge);
    border-radius: 6px;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
  }
  .search:focus {
    outline: none;
    border-color: var(--focus-ring);
  }
  .seg {
    display: inline-flex;
    align-self: flex-start;
    border: 1px solid var(--edge);
    border-radius: 6px;
    overflow: hidden;
  }
  .seg button {
    appearance: none;
    border: none;
    background: none;
    padding: 3px 12px;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: pointer;
  }
  .seg button + button {
    border-left: 1px solid var(--edge);
  }
  .seg button.on {
    color: var(--fg);
    background: var(--row-active);
  }
  .day {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .lbl {
    margin: 0;
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
    font-weight: 600;
  }
  .rows {
    display: flex;
    flex-direction: column;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 12px;
    border-radius: 5px;
  }
  .row:hover {
    background: var(--row-hover);
  }
  .main {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 6px 8px;
    border: none;
    background: none;
    color: var(--fg);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .title,
  .sub {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .title {
    font-size: var(--text-sm);
  }
  .sub {
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .rec.gone .title {
    color: var(--muted);
  }
  .hint {
    flex: none;
    font-size: var(--text-xs);
    color: var(--muted);
    opacity: 0;
    transition: opacity 0.12s ease;
  }
  .row:hover .hint,
  .main:focus-visible .hint {
    opacity: 1;
  }
  .meta {
    flex: none;
    display: flex;
    align-items: center;
    gap: 12px;
    padding-right: 8px;
    font-size: var(--text-xs);
    color: var(--muted);
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }
  .files {
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    color: inherit;
    cursor: pointer;
  }
  .files:hover,
  .files[aria-expanded="true"] {
    color: var(--fg);
  }
  .dur {
    min-width: 3.6em;
    text-align: right;
  }
  @container (max-width: 560px) {
    .meta > :not(.dur) {
      display: none;
    }
  }
  .filters {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .seg-one {
    appearance: none;
    border: 1px solid var(--edge);
    border-radius: 6px;
    background: none;
    padding: 3px 12px;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: pointer;
  }
  .seg-one.on {
    color: var(--fg);
    background: var(--row-active);
  }
  .main.static {
    cursor: default;
  }
  .act-btn {
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: pointer;
  }
  .act-btn:hover {
    color: var(--fg);
  }
  .act-btn:focus-visible {
    opacity: 1;
  }
  .quiet {
    margin: 0;
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .detail-box {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 2px 8px 12px 29px;
  }
  .facts {
    margin: 0;
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .commits {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
  }
  .commit {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    color: var(--fg);
    width: 100%;
    display: flex;
    align-items: baseline;
    gap: 10px;
    padding: 3px 6px;
    border-radius: 4px;
    text-align: left;
    cursor: pointer;
    font-size: var(--text-sm);
  }
  .commit:hover {
    background: var(--row-hover);
  }
  .csubj {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .csha {
    flex: none;
    color: var(--muted);
    font-family: var(--mono, monospace);
    font-size: var(--text-xs);
  }
  .acts {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
  }
  .act {
    display: flex;
    gap: 12px;
    padding: 6px 8px;
    font-size: var(--text-sm);
  }
  .act .when {
    flex: none;
    width: 9em;
    color: var(--muted);
    font-size: var(--text-xs);
    padding-top: 1px;
  }
  .act .what {
    min-width: 0;
  }
  .who {
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    color: var(--fg);
    font-weight: 600;
    cursor: pointer;
  }
  .who:hover {
    text-decoration: underline;
    text-underline-offset: 2px;
  }
  .act .detail {
    color: var(--muted);
  }
  .empty {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--muted);
    line-height: 1.5;
  }
  .err {
    color: var(--err);
  }
  .foot {
    display: flex;
    align-items: center;
    gap: 16px;
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .link {
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    color: var(--muted);
    cursor: pointer;
  }
  .link:hover:not(:disabled) {
    color: var(--fg);
  }
</style>
