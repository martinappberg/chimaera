<script lang="ts">
  /**
   * The workspace dashboard — the landing surface of a workspace: which
   * agents need you, what everyone is doing, what they produced, and where
   * you left off. Renders entirely from state the client already has (the
   * /ws/events roster, the git status store, the rail's recents) plus a
   * BOUNDED set of warm chat stores for inline permission answering and live
   * card detail. It is a router into live sessions, never a replacement —
   * every card is one click from the real pane. The Mastermind dock rides
   * along as a full-height third column (collapsing to an edge pill): the
   * one home of the workspace's privileged agent, which the roster below
   * deliberately never lists.
   */
  import { onMount, untrack } from "svelte";
  import { flip } from "svelte/animate";
  import BrandMark from "../shared/BrandMark.svelte";
  import SessionGlyph from "../shared/SessionGlyph.svelte";
  import AgentCard from "./AgentCard.svelte";
  import AttentionCard from "./AttentionCard.svelte";
  import MastermindDock from "./MastermindDock.svelte";
  import { acquireChat, releaseChat } from "../chat/chatPool";
  import type { ChatStore } from "../chat/store.svelte";
  import type { ChatSocket } from "../chat/chatWs";
  import { gitStatus } from "../workspace/git";
  import { computeStatus, formatSlurmDuration, parseSlurmTimeLeft } from "../workspace/compute";
  import { keyHint } from "../shared/keybindings";
  import { relativeAge } from "../workspace/launcher";
  import {
    agentKind,
    dotState,
    dotTitle,
    isBusy,
    needsAttention,
    type Session,
    isMastermind,
  } from "../workspace/sessions";
  import type { LayoutCtrl } from "../layout/dnd";
  import { rosterWeight, type DashCtx , relPath as sharedRelPath} from "./dash";

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

  // The Mastermind never joins the roster it observes (its flagged row lives
  // on the dock alone) — same filter App's wsSessions applies for the rail.
  const wsSessions = $derived(
    [...sessions.values()].filter((s) => s.workspace_id === wsId && !isMastermind(s)),
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

  /** Roster cards glide instead of teleporting when they REORDER within
   *  their list (the live-before-dead resort, a new sibling pushing the
   *  grid). `animate:flip` only tweens position deltas of the same keyed
   *  element inside ONE each-block, so a card crossing between the lane and
   *  the roster (an add here + a remove there) still cuts — that boundary is
   *  an attention-state change and reads fine as an instant move. Zero under
   *  reduced motion (flip has no media-query awareness). */
  const flipMs =
    typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches
      ? 0
      : 220;

  const nothingRunning = $derived(wsSessions.length === 0);

  // --- the Mastermind dock -------------------------------------------------------
  //
  // A bound Mastermind means the workspace is NOT "nothing running": the
  // dashboard chrome shows (dock + an honest empty roster) instead of the
  // launcher blank state. The true blank state offers "+ mastermind", which
  // opens the chrome with the dock's setup card visible.

  /** The Mastermind's roster row (looked up in the UNFILTERED map). */
  const mmSession = $derived(
    dash.mastermind !== null ? (sessions.get(dash.mastermind.session_id) ?? null) : null,
  );
  /** Honest pill affordance: an ask-mode Mastermind waiting on a permission
   *  must be findable while the dock is collapsed (wire state, no store). */
  const mmAttn = $derived(mmSession !== null && mmSession.alive && needsAttention(mmSession));

  /** The user asked for the setup card from the blank state. */
  let mastermindSetup = $state(false);
  const blank = $derived(nothingRunning && dash.mastermind === null && !mastermindSetup);

  // Collapse mechanism: the dock mounts/unmounts on toggle, so the width
  // gate is the PANE's own measured width (bind:clientWidth — a container
  // measure like the 860px activity-column query, never a window media
  // query). Wide panes get the docked third column; narrower ones a slim
  // edge pill that opens the dock as an overlay. Local state only (v1).
  let dashWidth = $state(0);
  const dockWide = $derived(dashWidth >= 1240);
  /** Wide-mode manual collapse (the dock header's » button). */
  let dockCollapsed = $state(false);
  /** Narrow-mode overlay toggle (the edge pill). */
  let overlayOpen = $state(false);
  const dockOverlay = $derived(!dockWide && overlayOpen);
  const showDock = $derived((dockWide && !dockCollapsed) || dockOverlay);

  // --- dock width: draggable, expandable to the whole surface ------------------
  //
  // The dock is a sidebar the user can pull out — anywhere from the slim
  // default to the FULL dashboard (the surface underneath is a summary; the
  // Mastermind conversation is sometimes the main event). Width persists
  // per browser profile (the rail-width idiom); expanded is a transient
  // focus mode, deliberately not persisted.
  const DOCK_MIN = 300;
  const DOCK_DEFAULT = 360;
  const DOCK_WIDTH_KEY = "chimaera.dashboard.dockWidth";
  let dockWidth = $state(DOCK_DEFAULT);
  let dockExpanded = $state(false);
  onMount(() => {
    const saved = Number(localStorage.getItem(DOCK_WIDTH_KEY));
    if (Number.isFinite(saved) && saved >= DOCK_MIN) dockWidth = Math.round(saved);
  });
  /** Clamp so the surface keeps a readable remainder — or goes full. */
  const dockMax = $derived(Math.max(DOCK_MIN, dashWidth - 320));
  const dockPx = $derived(Math.min(dockWidth, dockMax));

  let dockResizing = $state(false);
  function onDockResizeDown(e: PointerEvent): void {
    if (e.button !== 0) return;
    e.preventDefault();
    const handle = e.currentTarget as HTMLElement;
    handle.setPointerCapture(e.pointerId);
    dockResizing = true;
    const startX = e.clientX;
    const startW = dockExpanded ? dashWidth : dockPx;
    const move = (ev: PointerEvent) => {
      const w = startW + (startX - ev.clientX);
      // Dragging past the clamp toward the left edge snaps to full.
      dockExpanded = w > dockMax + 40;
      dockWidth = Math.min(Math.max(w, DOCK_MIN), dockMax);
    };
    const up = () => {
      dockResizing = false;
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", up);
      localStorage.setItem(DOCK_WIDTH_KEY, String(Math.round(dockWidth)));
    };
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", up);
  }
  function resetDockWidth(): void {
    dockExpanded = false;
    dockWidth = DOCK_DEFAULT;
    localStorage.setItem(DOCK_WIDTH_KEY, String(DOCK_DEFAULT));
  }

  function openDock(): void {
    if (dockWide) dockCollapsed = false;
    else overlayOpen = true;
  }
  function collapseDock(): void {
    if (dockWide) dockCollapsed = true;
    else overlayOpen = false;
  }
  function openMastermindSetup(): void {
    mastermindSetup = true;
    // The setup card must actually appear, whatever the pane width.
    dockCollapsed = false;
    overlayOpen = true;
  }

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
  // Attention-lane chat sessions are acquired first — their permission cards
  // answer inline over the live socket — then running chat sessions top up,
  // all under one shared cap. The pool refcounts holds (LRU never evicts a
  // held entry), so the cap here bounds how many sockets the dashboard adds,
  // not correctness; a lane past the cap still renders from wire state.
  const RICH_CAP = 4;
  const RICH_LANE_MAX = 8;
  const richIds = $derived.by(() => {
    const out: string[] = [];
    for (const s of lane) {
      if (out.length >= RICH_LANE_MAX) break;
      if (s.ui === "chat") out.push(s.id);
    }
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

  /** Stop a work row (subagent or background task — both ride stop_task).
   *  Same never-lose-a-click contract as decide(). */
  function stopTask(sessionId: string, taskId: string): void {
    const entry = rich.get(sessionId);
    if (entry === undefined) return;
    const sent = entry.socket.send({ type: "stop_task", task_id: taskId });
    if (!sent) entry.store.notice("not connected — stop not sent, try again", "error");
  }

  // --- the compute vital sign -----------------------------------------------------
  //
  // A scheduler is workspace context, not a queue table (decision 11): one
  // chip on the vital-signs strip, absent entirely when no scheduler exists.

  /** Local countdown baseline — ticks at 1 Hz ONLY inside an allocation
   *  (the ComputeStrip idiom: time_left moves per 60s fetch; the chip ticks
   *  against the snapshot's client receipt time). */
  let computeNow = $state(Date.now());
  $effect(() => {
    const snap = $computeStatus;
    if (snap === null || snap.self === null) return;
    computeNow = Date.now();
    const timer = setInterval(() => (computeNow = Date.now()), 1000);
    return () => clearInterval(timer);
  });

  /** The chip's text + tooltip; null = no scheduler (nothing renders). */
  const computeChip = $derived.by(() => {
    const snap = $computeStatus;
    if (snap === null || (snap.scheduler !== "slurm" && snap.self === null)) return null;
    if (snap.self !== null) {
      // Inside an allocation the walltime IS the vital sign.
      const title = `slurm job ${snap.self.job_id} on ${snap.self.node} — expires at walltime`;
      const baseline = parseSlurmTimeLeft(snap.self.time_left);
      if (baseline === null) {
        // Slurm's non-durations: a dash for the transitional placeholders,
        // the raw vocabulary otherwise (UNLIMITED is never relabeled).
        const raw =
          snap.self.time_left === "INVALID" || snap.self.time_left === "NOT_SET"
            ? "—"
            : snap.self.time_left;
        return { text: `slurm · ${raw}`, title };
      }
      const remaining = Math.max(0, baseline - Math.floor((computeNow - snap.received_at_ms) / 1000));
      return {
        text: remaining === 0 ? "slurm · expiring…" : `slurm · ${formatSlurmDuration(remaining)} left`,
        title,
      };
    }
    // Outside an allocation: the user's own queue, running vs pending
    // (queuedJobCount's filter, split per state — raw Slurm state words).
    const running = snap.jobs.filter((j) => j.state === "RUNNING").length;
    const pending = snap.jobs.filter((j) => j.state === "PENDING").length;
    return { text: `slurm · ${running} running · ${pending} pending`, title: "your slurm queue" };
  });

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

  const relPath = (p: string): string => sharedRelPath(wsRoot, p);

  const dirtyCount = $derived($gitStatus?.entries.length ?? 0);
</script>

<div class="dashboard" bind:clientWidth={dashWidth}>
  {#if !dash.ready}
    <div class="skeleton"><span>connecting…</span></div>
  {:else if blank}
    <!-- Nothing running AND no Mastermind: no dashboard chrome — the
         launcher-style blank state (per the design decision: an empty
         workspace shows nothing dashboard-shaped, just the ways to start).
         A bound Mastermind is something running: the chrome shows instead. -->
    <div class="blank">
      <BrandMark size={26} draw title="chimaera" />
      <h2>{dash.wsName}</h2>
      <p>Nothing running here yet.</p>
      <div class="blank-actions">
        <button class="cta" onclick={dash.onNewAgent}>+ new agent</button>
        <button class="cta quiet" onclick={dash.onNewTerminal}>+ terminal</button>
        <button
          class="cta quiet"
          title="one agent that watches the whole workspace and delegates work"
          onclick={openMastermindSetup}>+ mastermind</button
        >
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
    <!-- Chrome: the scrolling surface + the Mastermind dock as a full-height
         third column (or its collapsed edge pill) — flex siblings, so no
         horizontal scroll and no overlap at any pane width. -->
    <div class="body">
      <div class="scroll">
        <div class="inner">
      <!-- The vital-signs strip: name · branch · the summary sentence · the
           compute chip — one quiet row (decision 10). -->
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
        {#if computeChip !== null}
          <span class="compute" title={computeChip.title}>{computeChip.text}</span>
        {/if}
      </header>

      {#if continueTarget !== null && agents.length > 1}
        <!-- A shortcut, not a status: "last active" names what the row IS
             (the session you were in most recently) — the old bare
             "continue" label read as a stuck state/instruction. -->
        <button
          class="continue"
          title="jump back into your most recent session"
          onclick={() => dash.onOpenSession(continueTarget.id)}
        >
          <span class="dot {dotState(continueTarget)}" title={dotTitle(continueTarget)}></span>
          <span class="clabel">last active</span>
          <span class="cname">{names.get(continueTarget.id) ?? continueTarget.name}</span>
          <span class="cmeta"
            >{agentKind(continueTarget)} · {relativeAge(continueTarget.created_at)}</span
          >
          <span class="carrow" aria-hidden="true">↳</span>
        </button>
      {/if}

      <div class="columns">
        <div class="main">
          {#if nothingRunning}
            <!-- Chrome without workers (a Mastermind is bound, or its setup
                 was requested): say so honestly and keep the ways to start. -->
            <div class="noworkers">
              <p>No workers running yet.</p>
              <div class="blank-actions">
                <button class="cta" onclick={dash.onNewAgent}>+ new agent</button>
                <button class="cta quiet" onclick={dash.onNewTerminal}>+ terminal</button>
              </div>
            </div>
          {/if}

          {#if lane.length > 0}
            <div class="sec-title">needs you</div>
            <div class="lane">
              {#each lane as s (s.id)}
                <div animate:flip={{ duration: flipMs }}>
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
                </div>
              {/each}
            </div>
          {/if}

          {#if roster.length > 0}
            <div class="sec-title">{lane.length > 0 ? "the rest" : "agents"}</div>
            <div class="roster" class:compact>
              {#each roster as s (s.id)}
                <div class="cardwrap" animate:flip={{ duration: flipMs }}>
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
                </div>
              {/each}
            </div>
          {/if}

          {#if !nothingRunning}
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
          {/if}
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

          <!-- Recents live in the activity column, always (a stable place to
               pick up where you left off). They used to appear only in quiet
               moments, but that pop-in/out read as a glitch — a predictable
               always-there section is calmer than a clever one. -->
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
      </div>

      {#if wsId !== null}
        {#if showDock}
          <div
            class="dockcol"
            class:overlay={dockOverlay}
            class:expanded={dockExpanded}
            class:resizing={dockResizing}
            style:width={dockExpanded ? "100%" : `${dockPx}px`}
          >
            <!-- Pull the dock wider (up to the whole surface) — the rail-
                 resize idiom: drag the edge, double-click to reset. -->
            <div
              class="dock-resize"
              role="separator"
              aria-orientation="vertical"
              aria-label="resize the Mastermind dock"
              title="drag to resize · double-click to reset"
              onpointerdown={onDockResizeDown}
              ondblclick={resetDockWidth}
            ></div>
            <MastermindDock
              cfg={dash.mastermind}
              session={mmSession}
              {wsId}
              {paneId}
              {ctrl}
              refresh={dash.refreshWorkspaces}
              onCollapse={collapseDock}
              expanded={dockExpanded}
              onToggleExpand={() => (dockExpanded = !dockExpanded)}
            />
          </div>
        {:else}
          <button
            class="pill"
            title={dash.mastermind !== null
              ? "open the Mastermind dock"
              : "set up the Mastermind — one agent that watches the whole workspace"}
            onclick={openDock}
          >
            <BrandMark size={14} title="Mastermind" />
            <span class="pill-label">mastermind</span>
            {#if mmAttn}
              <span class="pill-dot" title="the Mastermind needs you"></span>
            {/if}
          </button>
        {/if}
      {/if}
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
  /* The chrome row: the scrolling surface + the Mastermind dock (or its edge
     pill) as flex siblings — the dock never overlaps content when docked,
     and only the deliberate narrow-width overlay ever floats above it. */
  .body {
    position: absolute;
    inset: 0;
    display: flex;
    min-width: 0;
  }
  .scroll {
    flex: 1;
    min-width: 0;
    overflow-y: auto;
  }

  .inner {
    max-width: 1000px;
    margin: 0 auto;
    padding: 22px 24px 40px;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }

  /* The Mastermind dock: a full-height third column right of the activity
     column, user-resizable up to the whole surface (width set inline).
     Narrow panes swap it for the edge pill; the pill opens it as an overlay
     pinned to the pane's right edge (see .dockcol.overlay). */
  .dockcol {
    position: relative;
    flex: none;
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
    border-left: 1px solid var(--edge);
    background: var(--bg);
  }
  .dockcol.overlay {
    position: absolute;
    top: 0;
    right: 0;
    bottom: 0;
    max-width: calc(100% - 44px);
    z-index: 6;
    box-shadow: -10px 0 32px rgba(0, 0, 0, 0.22);
  }
  .dockcol.overlay.expanded {
    max-width: 100%;
  }

  /* The width handle: a quiet splitter on the dock's left edge (the
     rail-resize idiom — invisible until hovered/dragged). */
  .dock-resize {
    position: absolute;
    top: 0;
    bottom: 0;
    left: -3px;
    width: 7px;
    cursor: col-resize;
    z-index: 7;
  }
  .dock-resize:hover,
  .dockcol.resizing .dock-resize {
    background: color-mix(in srgb, var(--accent) 30%, transparent);
  }

  /* The collapsed dock: a slim right-edge strip, always reachable. */
  .pill {
    flex: none;
    width: 30px;
    appearance: none;
    border: none;
    border-left: 1px solid var(--edge);
    background: none;
    color: var(--muted);
    cursor: pointer;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 8px;
    padding: 12px 0;
    font: inherit;
    transition:
      color 0.12s ease,
      background-color 0.12s ease;
  }
  .pill:hover {
    color: var(--fg);
    background: var(--row-hover);
  }
  .pill-label {
    writing-mode: vertical-rl;
    font-family: var(--mono);
    font-size: var(--text-xs);
    letter-spacing: 0.08em;
  }
  .pill-dot {
    flex: none;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--warn);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--warn) 16%, transparent);
  }

  /* Chrome without workers (a Mastermind is bound): honest, minimal. */
  .noworkers {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 10px;
    padding: 16px 16px 18px;
    border: 1px dashed var(--edge);
    border-radius: 8px;
    color: var(--muted);
    font-size: var(--text-md);
  }
  .noworkers p {
    margin: 0;
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
  /* The compute vital sign — the branch chip's shape with the accent's
     compute tint (the ComputeStrip family), quiet in both themes. */
  .compute {
    flex: none;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    border: 1px solid color-mix(in srgb, var(--accent) 30%, var(--edge));
    border-radius: 999px;
    padding: 1px 8px;
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
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
    font-size: var(--text-xs);
    color: var(--muted);
    letter-spacing: 0.04em;
  }
  .carrow {
    flex: none;
    margin-left: auto;
    color: var(--muted);
  }
  .continue:hover .carrow {
    color: var(--accent);
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
    /* auto-FIT (not auto-fill): a lone card fills the column instead of
       shrinking to 300px and stranding itself against a phantom track; two
       or three wrap into even columns. Cards cap their own line length via
       the container, so a single card never becomes a cavernous banner. */
    grid-template-columns: repeat(auto-fit, minmax(300px, 1fr));
    gap: 8px;
  }
  .roster.compact {
    grid-template-columns: minmax(0, 1fr);
    gap: 4px;
  }
  /* The flip-animation wrapper (animate: needs a keyed-each child); the
     card stretches to keep grid rows even. */
  .cardwrap {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .cardwrap > :global(.card) {
    flex: 1;
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
  /* Finished = a calm neutral ring (green is reserved for an active turn). */
  .dot.done {
    background: transparent;
    border: 1.5px solid var(--muted);
    opacity: 0.9;
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
