<script lang="ts">
  /**
   * The workspace dashboard — the landing surface of a workspace: which
   * agents need you, what everyone is doing, what they produced, and where
   * you left off. Renders entirely from state the client already has (the
   * /ws/events roster, the git status store, the rail's recents) plus a
   * BOUNDED set of warm chat stores for inline permission answering and live
   * card detail. It is a router into live sessions, never a replacement —
   * every card is one click from the real pane.
   */
  import { onMount, untrack } from "svelte";
  import BrandMark from "../shared/BrandMark.svelte";
  import SessionGlyph from "../shared/SessionGlyph.svelte";
  import AgentCard from "./AgentCard.svelte";
  import AttentionCard from "./AttentionCard.svelte";
  import { acquireChat, releaseChat } from "../chat/chatPool";
  import type { ChatStore } from "../chat/store.svelte";
  import type { ChatSocket } from "../chat/chatWs";
  import { gitStatus } from "../workspace/git";
  import { keyHint } from "../shared/keybindings";
  import { relativeAge } from "../workspace/launcher";
  import {
    agentKind,
    dotState,
    dotTitle,
    isBusy,
    needsAttention,
    type Session,
  } from "../workspace/sessions";
  import type { LayoutCtrl } from "../layout/dnd";
  import { rosterWeight, type DashCtx } from "./dash";

  interface Props {
    dash: DashCtx;
    sessions: Map<string, Session>;
    names: Map<string, string>;
    wsId: string | null;
    wsRoot: string | null;
    paneId: string;
    ctrl: LayoutCtrl;
  }

  let { dash, sessions, names, wsId, wsRoot, paneId, ctrl }: Props = $props();

  // --- the roster --------------------------------------------------------------

  const wsSessions = $derived(
    [...sessions.values()].filter((s) => s.workspace_id === wsId),
  );
  const agents = $derived(wsSessions.filter((s) => s.kind === "agent"));
  const shells = $derived(wsSessions.filter((s) => s.kind !== "agent"));

  /** Live asks, ranked into the lane; dead sessions keep to the roster. */
  const lane = $derived(agents.filter((s) => s.alive && needsAttention(s)));
  const roster = $derived(
    agents
      .filter((s) => !(s.alive && needsAttention(s)))
      .toSorted((a, b) => rosterWeight(a) - rosterWeight(b) || b.created_at - a.created_at),
  );

  const working = $derived(agents.filter((s) => s.alive && s.agent_state === "running").length);
  const finished = $derived(agents.filter((s) => s.agent_state === "finished").length);
  const busyShells = $derived(shells.filter(isBusy).length);

  const hero = $derived(lane.length === 0 && roster.length === 1);
  const compact = $derived(roster.length >= 7);

  const nothingRunning = $derived(wsSessions.length === 0);

  /** "Continue where you left off": this window's most recent agent, else the
   *  newest live one. Rendered only when it isn't already the whole story. */
  const continueTarget = $derived.by(() => {
    for (const id of dash.mru) {
      const s = sessions.get(id);
      if (s !== undefined && s.alive && s.workspace_id === wsId && s.kind === "agent") return s;
    }
    return agents.filter((s) => s.alive).toSorted((a, b) => b.created_at - a.created_at)[0] ?? null;
  });

  // --- bounded rich detail (warm chat stores) -----------------------------------
  //
  // Attention-lane chat sessions are always acquired — their permission cards
  // answer inline over the live socket. Beyond those, running chat sessions
  // get rich detail only while the total stays small (RICH_CAP), so the
  // dashboard can never churn the chat pool's LRU out from under open tabs.
  const RICH_CAP = 4;
  const richIds = $derived.by(() => {
    const out: string[] = [];
    for (const s of lane) if (s.ui === "chat") out.push(s.id);
    for (const s of roster) {
      if (out.length >= RICH_CAP) break;
      if (s.ui === "chat" && s.alive) out.push(s.id);
    }
    return out;
  });

  let rich = $state(new Map<string, { store: ChatStore; socket: ChatSocket }>());
  $effect(() => {
    const want = richIds;
    untrack(() => {
      let changed = false;
      const next = new Map(rich);
      for (const id of want) {
        if (!next.has(id)) {
          next.set(id, acquireChat(id));
          changed = true;
        }
      }
      for (const id of [...next.keys()]) {
        if (!want.includes(id)) {
          next.delete(id);
          releaseChat(id);
          changed = true;
        }
      }
      if (changed) rich = next;
    });
  });
  onMount(() => () => {
    for (const id of rich.keys()) releaseChat(id);
  });

  function decide(
    sessionId: string,
    requestId: string,
    optionId: string,
    destination?: string,
    feedback?: string,
  ): void {
    const entry = rich.get(sessionId);
    if (entry === undefined) return;
    const sent = entry.socket.send({
      type: "permission",
      request_id: requestId,
      option_id: optionId,
      ...(destination !== undefined ? { destination } : {}),
      ...(feedback !== undefined ? { feedback } : {}),
    });
    // Never lose a decision to a closed socket: the card stays answerable.
    if (!sent) entry.store.notice("not connected — decision not sent, try again", "error");
  }

  function stopTask(sessionId: string, taskId: string): void {
    rich.get(sessionId)?.socket.send({ type: "stop_task", task_id: taskId });
  }

  // --- the activity column -------------------------------------------------------

  interface TouchedRow {
    path: string;
    /** The agent that reported the write, or null (uncommitted, unattributed). */
    by: Session | null;
  }
  /** Newest-first union of agent-reported writes, then uncommitted paths no
   *  agent claimed (attribution is best-effort — the caveat rides the chip). */
  const changedFiles = $derived.by(() => {
    const rows: TouchedRow[] = [];
    for (const s of agents) {
      for (const p of s.files_touched ?? []) rows.push({ path: p, by: s });
    }
    rows.reverse();
    const seen = new Set<string>();
    const out: TouchedRow[] = [];
    for (const r of rows) {
      if (seen.has(r.path)) continue;
      seen.add(r.path);
      out.push(r);
    }
    const root = wsRoot ?? "";
    for (const e of $gitStatus?.entries ?? []) {
      const abs = e.path.startsWith("/") ? e.path : `${root}/${e.path}`;
      if (seen.has(abs)) continue;
      seen.add(abs);
      out.push({ path: abs, by: null });
    }
    return out.slice(0, 10);
  });

  function relPath(p: string): string {
    return wsRoot !== null && p.startsWith(`${wsRoot}/`) ? p.slice(wsRoot.length + 1) : p;
  }

  const dirtyCount = $derived($gitStatus?.entries.length ?? 0);
</script>

<div class="dashboard">
  {#if !dash.ready}
    <div class="skeleton"><span>connecting…</span></div>
  {:else if nothingRunning}
    <!-- Nothing running: no dashboard chrome — the launcher-style blank
         state (per the design decision: an empty workspace shows nothing
         dashboard-shaped, just the ways to start). -->
    <div class="blank">
      <BrandMark size={26} draw title="chimaera" />
      <h2>{dash.wsName}</h2>
      <p>Nothing running here yet.</p>
      <div class="blank-actions">
        <button class="cta" onclick={dash.onNewAgent}>+ new agent</button>
        <button class="cta quiet" onclick={dash.onNewTerminal}>+ terminal</button>
      </div>
      {#if dash.recents.length > 0}
        <div class="blank-recents">
          <div class="sec-title">pick up where you left off</div>
          {#each dash.recents.slice(0, 5) as r (r.resume ?? `${r.kind}:${r.title}`)}
            <button class="rrow" onclick={() => dash.onOpenRecent(r)}>
              <SessionGlyph kind="agent" agentKind={r.kind} size={11} />
              <span class="rtitle">{r.title}</span>
              <span class="rage">{relativeAge(r.lastActive)}</span>
            </button>
          {/each}
        </div>
      {/if}
      <p class="hint"><kbd>{keyHint("quickOpen")}</kbd> to open a file</p>
    </div>
  {:else}
    <div class="inner">
      <header class="strip">
        <span class="wsname">{dash.wsName}</span>
        {#if $gitStatus !== null}
          <span class="branch" title="current branch">
            {$gitStatus.branch ?? ($gitStatus.detached ? "detached" : "—")}
          </span>
        {/if}
        <span class="sentence">
          {#if working > 0}<b class="w">{working} working</b>{/if}
          {#if lane.length > 0}<b class="a">{lane.length} waiting on you</b>{/if}
          {#if finished > 0}<b class="d">{finished} finished</b>{/if}
          {#if working === 0 && lane.length === 0 && finished === 0}
            <b class="d">all quiet</b>
          {/if}
        </span>
      </header>

      {#if continueTarget !== null && agents.length > 1}
        <button class="continue" onclick={() => dash.onOpenSession(continueTarget.id)}>
          <span class="dot {dotState(continueTarget)}" title={dotTitle(continueTarget)}></span>
          <span class="clabel">continue</span>
          <span class="cname">{names.get(continueTarget.id) ?? continueTarget.name}</span>
          <span class="cmeta"
            >{agentKind(continueTarget)} · {relativeAge(continueTarget.created_at)}</span
          >
        </button>
      {/if}

      <div class="columns">
        <div class="main">
          {#if lane.length > 0}
            <div class="sec-title">needs you</div>
            <div class="lane">
              {#each lane as s (s.id)}
                <AttentionCard
                  session={s}
                  name={names.get(s.id) ?? s.name}
                  store={rich.get(s.id)?.store ?? null}
                  onOpen={() => dash.onOpenSession(s.id)}
                  onDecide={s.ui === "chat"
                    ? (requestId, optionId, destination, feedback) =>
                        decide(s.id, requestId, optionId, destination, feedback)
                    : undefined}
                />
              {/each}
            </div>
          {/if}

          {#if roster.length > 0}
            <div class="sec-title">{lane.length > 0 ? "the rest" : "agents"}</div>
            <div class="roster" class:compact>
              {#each roster as s (s.id)}
                <AgentCard
                  session={s}
                  name={names.get(s.id) ?? s.name}
                  store={rich.get(s.id)?.store ?? null}
                  {compact}
                  hero={hero && s.alive}
                  {wsRoot}
                  onOpen={() => dash.onOpenSession(s.id)}
                  onOpenChanges={() => ctrl.openChangesFrom(paneId, s.id, false)}
                  onStopTask={s.ui === "chat" && agentKind(s) === "claude"
                    ? (taskId) => stopTask(s.id, taskId)
                    : undefined}
                />
              {/each}
            </div>
          {/if}

          <div class="shells">
            {#if shells.length > 0}
              <span class="shellsum">
                {shells.length} terminal{shells.length === 1 ? "" : "s"}
                {#if busyShells > 0}· {busyShells} running a command{/if}
              </span>
              {#each shells.slice(0, 4) as t (t.id)}
                <button class="shellchip" onclick={() => dash.onOpenSession(t.id)}>
                  <span class="dot {dotState(t)}" title={dotTitle(t)}></span>
                  {names.get(t.id) ?? t.name}
                </button>
              {/each}
            {/if}
            <button class="ghost" onclick={dash.onNewTerminal}>+ terminal</button>
            <button class="ghost" onclick={dash.onNewAgent}>+ agent</button>
          </div>
        </div>

        <aside class="side">
          {#if changedFiles.length > 0}
            <div class="sec">
              <div class="sec-title">changed files</div>
              {#each changedFiles as f (f.path)}
                <button
                  class="frow"
                  title={f.path}
                  onclick={() => ctrl.openFileFrom(paneId, f.path, false)}
                >
                  <span class="fpath">&#8206;{relPath(f.path)}</span>
                  {#if f.by !== null}
                    <span class="fby" title="last written by {names.get(f.by.id) ?? f.by.name}"
                      >{agentKind(f.by)}</span
                    >
                  {:else}
                    <span
                      class="fby quiet"
                      title="uncommitted change no agent reported — attribution is best-effort"
                      >you</span
                    >
                  {/if}
                </button>
              {/each}
            </div>
          {/if}

          {#if dash.recents.length > 0}
            <div class="sec">
              <div class="sec-title">recents</div>
              {#each dash.recents.slice(0, 5) as r (r.resume ?? `${r.kind}:${r.title}`)}
                <button class="rrow" onclick={() => dash.onOpenRecent(r)}>
                  <SessionGlyph kind="agent" agentKind={r.kind} size={11} />
                  <span class="rtitle">{r.title}</span>
                  <span class="rage">{relativeAge(r.lastActive)}</span>
                </button>
              {/each}
            </div>
          {/if}

          {#if $gitStatus !== null}
            <div class="sec">
              <div class="sec-title">git</div>
              <button class="grow" onclick={dash.onOpenGit} title="open source control">
                <span class="gbranch">{$gitStatus.branch ?? "detached"}</span>
                {#if $gitStatus.ahead > 0}<span class="gnum">↑{$gitStatus.ahead}</span>{/if}
                {#if $gitStatus.behind > 0}<span class="gnum">↓{$gitStatus.behind}</span>{/if}
                <span class="gdirty"
                  >{dirtyCount === 0 ? "clean" : `${dirtyCount} change${dirtyCount === 1 ? "" : "s"}`}</span
                >
              </button>
            </div>
          {/if}
        </aside>
      </div>
    </div>
  {/if}
</div>

<style>
  .dashboard {
    position: absolute;
    inset: 0;
    overflow-y: auto;
    background: var(--bg);
    container-type: inline-size;
  }

  .skeleton {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 100%;
    color: var(--muted);
    font-size: var(--text-sm);
  }

  /* --- blank state ----------------------------------------------------------- */
  .blank {
    min-height: 100%;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 12px;
    padding: 32px 24px;
    color: var(--muted);
  }
  .blank h2 {
    margin: 0;
    font-size: 17px;
    font-weight: 600;
    color: var(--fg);
    letter-spacing: 0.01em;
  }
  .blank p {
    margin: 0;
    font-size: var(--text-md);
  }
  .blank-actions {
    display: flex;
    gap: 8px;
    margin-top: 4px;
  }
  .cta {
    appearance: none;
    border: 1px solid var(--edge);
    background: var(--overlay-bg);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-md);
    padding: 6px 14px;
    border-radius: 6px;
    cursor: pointer;
    transition: border-color 0.12s ease;
  }
  .cta:hover {
    border-color: var(--accent);
  }
  .cta.quiet {
    color: var(--muted);
  }
  .cta.quiet:hover {
    color: var(--fg);
  }
  .blank-recents {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin-top: 14px;
    min-width: min(340px, 90%);
  }
  .hint {
    margin-top: 10px;
    font-size: var(--text-sm);
    opacity: 0.8;
  }
  kbd {
    font-family: var(--mono);
    font-size: 10px;
    color: var(--muted);
    border: 1px solid var(--edge);
    border-radius: 3px;
    padding: 0 3px;
  }

  /* --- the populated surface --------------------------------------------------- */
  .inner {
    max-width: 1000px;
    margin: 0 auto;
    padding: 22px 24px 40px;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }

  .strip {
    display: flex;
    align-items: baseline;
    gap: 12px;
    min-width: 0;
    flex-wrap: wrap;
  }
  .wsname {
    font-size: var(--text-lg);
    font-weight: 600;
  }
  .branch {
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--git-modified);
    border: 1px solid color-mix(in srgb, var(--git-modified) 35%, transparent);
    border-radius: 999px;
    padding: 1px 8px;
    max-width: 220px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .sentence {
    margin-left: auto;
    display: flex;
    gap: 10px;
    font-size: var(--text-sm);
    color: var(--muted);
  }
  .sentence b {
    font-weight: 500;
  }
  .sentence .w {
    color: var(--accent);
  }
  .sentence .a {
    color: var(--warn);
  }
  .sentence .d {
    color: var(--muted);
  }

  .continue {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    padding: 7px 10px;
    border: 1px solid var(--edge);
    border-radius: 6px;
    background: var(--overlay-bg);
    font: inherit;
    font-size: var(--text-sm);
    color: var(--muted);
    cursor: pointer;
    text-align: left;
    transition: border-color 0.12s ease;
  }
  .continue:hover {
    border-color: var(--accent);
  }
  .clabel {
    flex: none;
  }
  .cname {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono);
    color: var(--fg);
  }
  .cmeta {
    margin-left: auto;
    flex: none;
    font-family: var(--mono);
    font-size: var(--text-xs);
    opacity: 0.8;
  }

  .columns {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 264px;
    gap: 18px;
    align-items: start;
  }
  @container (max-width: 860px) {
    .columns {
      grid-template-columns: minmax(0, 1fr);
    }
  }

  .main {
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-width: 0;
  }

  .sec-title {
    font-size: var(--text-xs);
    color: var(--muted);
    letter-spacing: 0.04em;
    text-transform: lowercase;
    padding: 4px 2px 2px;
  }

  .lane {
    display: flex;
    flex-direction: column;
    gap: 8px;
    border-left: 2px solid color-mix(in srgb, var(--warn) 55%, transparent);
    padding-left: 10px;
    margin-bottom: 6px;
  }

  .roster {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(300px, 1fr));
    gap: 8px;
  }
  .roster.compact {
    grid-template-columns: minmax(0, 1fr);
    gap: 4px;
  }

  .shells {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    padding: 8px 2px 0;
    font-size: var(--text-sm);
    color: var(--muted);
  }
  .shellsum {
    font-size: var(--text-sm);
  }
  .shellchip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    border: 1px solid var(--edge);
    background: none;
    color: var(--muted);
    font: inherit;
    font-family: var(--mono);
    font-size: var(--text-xs);
    border-radius: 999px;
    padding: 1px 8px;
    cursor: pointer;
  }
  .shellchip:hover {
    color: var(--fg);
    border-color: color-mix(in srgb, var(--accent) 45%, var(--edge));
  }
  .ghost {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: pointer;
    padding: 2px 4px;
    border-radius: 4px;
  }
  .ghost:hover {
    color: var(--fg);
  }

  /* Shared dot vocabulary (rail semantics). */
  .dot {
    flex: none;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--muted);
    opacity: 0.4;
  }
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
    background: transparent;
    border: 1.5px solid var(--accent);
    opacity: 0.8;
  }
  .dot.idle {
    opacity: 0.55;
  }

  /* --- the activity column -------------------------------------------------- */
  .side {
    display: flex;
    flex-direction: column;
    gap: 14px;
    min-width: 0;
  }
  .sec {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }

  .frow,
  .rrow,
  .grow {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    padding: 3px 6px;
    border: none;
    background: none;
    font: inherit;
    color: var(--fg);
    text-align: left;
    border-radius: 5px;
    cursor: pointer;
  }
  .frow:hover,
  .rrow:hover,
  .grow:hover {
    background: var(--row-hover);
  }

  .fpath {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    direction: rtl;
    text-align: left;
    font-family: var(--mono);
    font-size: var(--text-sm);
  }
  .fby {
    flex: none;
    font-family: var(--mono);
    font-size: 10px;
    color: var(--muted);
    border: 1px solid var(--edge);
    border-radius: 999px;
    padding: 0 6px;
  }
  .fby.quiet {
    opacity: 0.7;
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
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    opacity: 0.8;
  }

  .grow {
    font-family: var(--mono);
    font-size: var(--text-sm);
  }
  .gbranch {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .gnum {
    flex: none;
    color: var(--muted);
    font-size: var(--text-xs);
  }
  .gdirty {
    margin-left: auto;
    flex: none;
    color: var(--muted);
    font-size: var(--text-xs);
  }
</style>
