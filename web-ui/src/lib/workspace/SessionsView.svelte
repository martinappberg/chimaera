<script lang="ts">
  /**
   * All sessions: every past session of the workspace, from its history
   * records (`GET /workspaces/{id}/history`) — date, duration, agent and
   * model, who started it, files changed, commits (in a repository) and
   * cost. Search by title and first prompt, filter by agent; a row opens to
   * its details, its edits, and a way back in while the agent still has the
   * conversation (the Recents resume flow) — or says plainly why not. The
   * Mastermind's actions are one toggle away. Pulled while visible, and
   * again on the recents nudge (a session ended); never polled.
   */
  import { untrack } from "svelte";
  import type { DashCtx } from "../dashboard/dash";
  import type { LayoutCtrl } from "../layout/dnd";
  import { ApiError } from "../net/api";
  import SessionGlyph from "../shared/SessionGlyph.svelte";
  import FileIcon from "../shared/FileIcon.svelte";
  import type { Session } from "./sessions";
  import {
    commitCount,
    fetchHistory,
    formatCost,
    formatDuration,
    formatTokens,
    historyNudge,
    recordTitle,
    startedByLabel,
    type HistoryAct,
    type HistoryRecord,
  } from "./history";
  import SessionEdits from "./SessionEdits.svelte";

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

  let query = $state("");
  /** The query the list was fetched with (debounced). */
  let applied = $state("");
  let agent = $state("");
  let showActs = $state(false);
  let expanded = $state<string | null>(null);

  // Typing settles for a beat before a fetch.
  $effect(() => {
    const q = query;
    const t = setTimeout(() => (applied = q), 250);
    return () => clearTimeout(t);
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
      const page = await fetchHistory(ws, {
        before,
        q: applied,
        agent,
        limit: PAGE,
        acts: reset && showActs,
      });
      if (mine !== seq) return;
      records = reset ? page.records : [...records, ...page.records];
      more = page.more;
      if (reset) acts = page.acts ?? [];
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
    void showActs;
    void $historyNudge;
    untrack(() => void load(true));
  });

  /** Agents present in what's loaded (the filter chips), plus the active one. */
  const agents = $derived.by(() => {
    const set = new Set(records.map((r) => r.agent));
    if (agent !== "") set.add(agent);
    return [...set].sort();
  });

  /** Day groups, newest first (the Timeline's vocabulary). */
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
              weekday: "short",
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
    const live = names.get(id) ?? sessions.get(id)?.display_name ?? undefined;
    if (live) return live;
    const rec = records.find((r) => r.id === id);
    return rec !== undefined ? recordTitle(rec) : undefined;
  }

  function clock(ms: number): string {
    return new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  }

  function model(r: HistoryRecord): string | null {
    return r.models?.at(-1) ?? null;
  }

  function reopen(r: HistoryRecord): void {
    if (r.live) {
      dash.onOpenSession(r.id);
      return;
    }
    const resume = r.reopen?.resume ?? null;
    if (resume === null) return;
    dash.onOpenRecent({
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

  const openFile = (p: string, e: MouseEvent) => {
    const abs = wsRoot !== null && !p.startsWith("/") ? `${wsRoot}/${p}` : p;
    ctrl.openFileFrom(paneId, abs, e.metaKey || e.ctrlKey);
  };

  const ACT_WORDS: Record<string, string> = {
    spawn_agent: "started",
    spawn_terminal: "opened the terminal",
    message_agent: "messaged",
    interrupt_agent: "stopped the turn of",
    deliver_note: "delivered a note to",
    wake_mastermind: "woke",
  };

  /** The Mastermind's acts name it by role, whatever its session is called. */
  const MASTERMIND_ACTS = new Set(["spawn_agent", "spawn_terminal", "message_agent", "interrupt_agent"]);

  function who(id: string): string {
    if (id === dash.mastermind?.session_id) return "the Mastermind";
    return nameOf(id) ?? id;
  }

  function actor(a: HistoryAct): string {
    if (a.by === "you") return "You";
    if (MASTERMIND_ACTS.has(a.act)) return "The Mastermind";
    return who(a.by);
  }
</script>

<div class="sessions">
  <div class="inner">
    <header class="head">
      <div class="titles">
        <h1>All sessions</h1>
        <p class="sub">Every agent session in this workspace — kept by chimaera, with or without git.</p>
      </div>
      <div class="tools">
        <input class="search" type="search" placeholder="Search titles and first prompts" bind:value={query} aria-label="search sessions" />
      </div>
    </header>

    <div class="chips" role="group" aria-label="Show">
      <button class="chip" class:on={agent === "" && !showActs} aria-pressed={agent === "" && !showActs} onclick={() => ((agent = ""), (showActs = false))}>all</button>
      {#each agents as a (a)}
        <button class="chip" class:on={agent === a && !showActs} aria-pressed={agent === a && !showActs} onclick={() => ((agent = a), (showActs = false))}>{a}</button>
      {/each}
      <button class="chip" class:on={showActs} aria-pressed={showActs} onclick={() => (showActs = !showActs)}>Mastermind actions</button>
    </div>

    {#if error !== null && records.length === 0}
      <p class="empty err">{error}</p>
    {:else if showActs}
      {#if acts.length === 0}
        <p class="empty">{loading ? "loading…" : "No Mastermind actions or note deliveries recorded here."}</p>
      {:else}
        <ol class="acts">
          {#each acts as a, i (`${a.ts}:${i}`)}
            <li class="act">
              <span class="when">{new Date(a.ts).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" })}</span>
              <span class="what">
                <b>{actor(a)}</b>
                {ACT_WORDS[a.act] ?? a.act}
                {#if a.target !== undefined}<b>{who(a.target)}</b>{/if}
                {#if a.detail !== undefined}<span class="detail">— {a.detail}</span>{/if}
              </span>
            </li>
          {/each}
        </ol>
      {/if}
    {:else if records.length === 0}
      <p class="empty">
        {#if !loaded || loading}loading…{:else if applied !== "" || agent !== ""}No session matches.{:else}No sessions recorded yet. Every agent session started from now on lands here when it ends.{/if}
      </p>
    {:else}
      {#each days as day (day.key)}
        <section class="day" aria-label={day.label}>
          <h2 class="lbl">{day.label}</h2>
          <div class="rows">
            {#each day.rows as r (r.rid)}
              {@const isOpen = expanded === r.rid}
              {@const commits = commitCount(r)}
              <div class="rec" class:open={isOpen}>
                <button class="row" aria-expanded={isOpen} onclick={() => toggle(r)}>
                  <span class="time">{clock(r.started)}</span>
                  <SessionGlyph kind="agent" agentKind={r.agent} size={11} title={r.agent} />
                  <span class="main">
                    <span class="title">{recordTitle(r)}</span>
                    {#if r.first_prompt !== undefined && r.first_prompt !== recordTitle(r)}
                      <span class="prompt">{r.first_prompt}</span>
                    {/if}
                  </span>
                  <span class="meta">
                    {#if r.live}
                      <span class="tag live">running</span>
                    {:else if r.outcome === "crashed"}
                      <span class="tag crash">crashed</span>
                    {:else if r.outcome === "retired"}
                      <span class="tag" title="the daemon stopped; a resumed session continues in its own row">stopped by a restart</span>
                    {/if}
                    {#if r.mastermind}<span class="tag">Mastermind</span>{/if}
                    <span class="m" title="duration">{r.ended !== undefined ? formatDuration(r.ended - r.started) : formatDuration(Date.now() - r.started)}</span>
                    {#if model(r) !== null}<span class="m mono" title="model">{model(r)}</span>{/if}
                    <span class="m" title="started by">by {startedByLabel(r.started_by, nameOf)}</span>
                    <span class="m" title="files written">{r.files.n} file{r.files.n === 1 ? "" : "s"}</span>
                    {#if commits !== null}<span class="m" title="commits made during the session">{commits} commit{commits === 1 ? "" : "s"}</span>{/if}
                    <span class="m cost" title={r.usage.cost_usd === null ? "this agent reports no cost" : "estimated at API prices"}>{formatCost(r.usage.cost_usd)}</span>
                  </span>
                </button>
                {#if isOpen}
                  <div class="detail-box">
                    <div class="facts">
                      <span>{new Date(r.started).toLocaleString()}{r.ended !== undefined ? ` – ${clock(r.ended)}` : ""}</span>
                      <span>{r.agent}{r.ui === "chat" ? " chat" : " terminal"}{(r.models ?? []).length > 0 ? ` · ${(r.models ?? []).join(", ")}` : ""}</span>
                      <span>
                        {r.usage.turns !== null ? `${r.usage.turns} turn${r.usage.turns === 1 ? "" : "s"} · ` : ""}{formatCost(r.usage.cost_usd)}
                        <span class="basis">estimated at API prices</span>
                        · {formatTokens(r.usage.tokens_in)} in / {formatTokens(r.usage.tokens_out)} out
                      </span>
                    </div>
                    <div class="actions">
                      {#if r.live}
                        <button class="opt primary" onclick={() => reopen(r)}>open</button>
                      {:else if r.reopen?.resume}
                        <button class="opt primary" onclick={() => reopen(r)}>resume</button>
                      {:else if r.reopen?.gone}
                        <span class="gone">{r.reopen.gone}</span>
                      {/if}
                    </div>
                    {#if (r.files.top ?? []).length > 0}
                      <div class="written">
                        <span class="lbl">Files written</span>
                        {#each r.files.top ?? [] as p (p)}
                          <button class="file" title="open {p}" onclick={(e) => openFile(p, e)}>
                            <FileIcon path={p} size={13} />
                            <span>{p}</span>
                          </button>
                        {/each}
                        {#if r.files.n > (r.files.top ?? []).length}
                          <span class="more">and {r.files.n - (r.files.top ?? []).length} more</span>
                        {/if}
                      </div>
                    {/if}
                    {#if wsId !== null}
                      <SessionEdits sessionId={r.id} {wsId} {wsRoot} {visible} onOpenFile={(p, e) => ctrl.openFileFrom(paneId, p, e.metaKey || e.ctrlKey)} />
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
          <button class="opt quiet" disabled={loading} onclick={() => void load(false)}>{loading ? "loading…" : "load older"}</button>
        {:else}
          <span class="end">the beginning of what chimaera kept</span>
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
    max-width: 980px;
    margin: 0 auto;
    padding: 26px 32px 48px;
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  .head {
    display: flex;
    align-items: flex-end;
    gap: 20px;
    flex-wrap: wrap;
  }
  .titles {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
  }
  h1 {
    margin: 0;
    font-size: 22px;
    font-weight: 600;
    letter-spacing: -0.01em;
  }
  .sub {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--muted);
  }
  .tools {
    margin-left: auto;
  }
  .search {
    width: 260px;
    max-width: 100%;
    padding: 5px 10px;
    border: 1px solid var(--edge);
    border-radius: 6px;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
  }
  .search:focus {
    outline: none;
    border-color: var(--accent);
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .chip {
    appearance: none;
    border: 1px solid var(--edge);
    background: none;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-xs);
    padding: 3px 11px;
    border-radius: 999px;
    cursor: pointer;
    transition:
      color 0.12s ease,
      border-color 0.12s ease;
  }
  .chip:hover {
    color: var(--fg);
  }
  .chip.on {
    color: var(--fg);
    border-color: var(--fg);
  }
  .day {
    display: flex;
    flex-direction: column;
    gap: 6px;
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
    border-bottom: 1px solid var(--edge);
  }
  .rec {
    border-top: 1px solid var(--edge);
  }
  .row {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 6px;
    border: none;
    background: none;
    color: var(--fg);
    font: inherit;
    text-align: left;
    cursor: pointer;
    border-radius: 5px;
  }
  .row:hover {
    background: var(--row-hover);
  }
  .time {
    flex: none;
    min-width: 5.2em;
    white-space: nowrap;
    font-size: var(--text-xs);
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }
  .main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .title,
  .prompt {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .title {
    font-size: var(--text-sm);
  }
  .prompt {
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .meta {
    flex: none;
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: var(--text-xs);
    color: var(--muted);
    white-space: nowrap;
  }
  .m.mono {
    font-family: var(--mono, monospace);
    max-width: 12em;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .m.cost {
    min-width: 3.6em;
    text-align: right;
    color: var(--fg);
    font-variant-numeric: tabular-nums;
  }
  .tag {
    padding: 0 6px;
    border: 1px solid var(--edge);
    border-radius: 999px;
    font-size: 10.5px;
  }
  .tag.live {
    color: var(--accent);
    border-color: color-mix(in srgb, var(--accent) 45%, var(--edge));
  }
  .tag.crash {
    color: var(--err);
    border-color: color-mix(in srgb, var(--err) 45%, var(--edge));
  }
  @container (max-width: 640px) {
    .meta .m:not(.cost) {
      display: none;
    }
  }
  .detail-box {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 4px 8px 14px calc(5.2em + 33px);
  }
  .facts {
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: var(--text-sm);
    color: var(--muted);
  }
  .basis {
    font-size: var(--text-xs);
    opacity: 0.85;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .gone {
    font-size: var(--text-sm);
    color: var(--muted);
  }
  .written {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 2px;
  }
  .file {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 2px 6px;
    border: none;
    border-radius: 4px;
    background: none;
    color: var(--fg);
    font-family: var(--mono, monospace);
    font-size: var(--text-sm);
    cursor: pointer;
  }
  .file:hover {
    background: var(--row-hover);
  }
  .more {
    font-size: var(--text-xs);
    color: var(--muted);
    padding-left: 6px;
  }
  .acts {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    border-top: 1px solid var(--edge);
  }
  .act {
    display: flex;
    gap: 12px;
    padding: 7px 6px;
    border-bottom: 1px solid var(--edge);
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
  .act b {
    font-weight: 600;
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
    gap: 12px;
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .end {
    opacity: 0.8;
  }
</style>
