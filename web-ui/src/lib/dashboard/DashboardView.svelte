<script lang="ts">
  /**
   * The workspace dashboard — re-centred on questions (design §3): what
   * needs me (the attention lane), what happened while I was away (the
   * Timeline since this viewer's last look), where the project stands (the
   * top of Knowledge), and who is running (one line; cards on request).
   * Renders from state the client already has (the /ws/events roster, the
   * git/timeline/knowledge/plugin stores) plus a BOUNDED set of warm chat
   * stores for inline permission answering. It is a router into live
   * sessions and the Timeline/Knowledge views, never a replacement. The
   * Mastermind lives in the window's own panel (MastermindPanel — every
   * view, one per window); the dashboard only offers the quiet way in, and
   * the roster deliberately never lists it.
   */
  import { onMount, untrack } from "svelte";
  import { flip } from "svelte/animate";
  import BrandMark from "../shared/BrandMark.svelte";
  import SessionGlyph from "../shared/SessionGlyph.svelte";
  import AgentCard from "./AgentCard.svelte";
  import AttentionCard from "./AttentionCard.svelte";
  import { mastermindPanel, setMastermindPanelOpen } from "./mastermindPanelState.svelte";
  import NowLine from "./NowLine.svelte";
  import SinceYouLeft from "./SinceYouLeft.svelte";
  import WhereThingsStand from "./WhereThingsStand.svelte";
  import { acquireChat, releaseChat } from "../chat/chatPool";
  import type { ChatStore } from "../chat/store.svelte";
  import type { ChatSocket } from "../chat/chatWs";
  import { gitStatus } from "../workspace/git";
  import { computeStatus, formatSlurmDuration, parseSlurmTimeLeft } from "../workspace/compute";
  import { keyHint } from "../shared/keybindings";
  import { pageVisible } from "../shared/visibility";
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
  import { rosterWeight, type DashCtx } from "./dash";
  import { getSetting, setSetting } from "../settings/store.svelte";
  import { lastSeen, markSeen, timelineStore, type SeenMark } from "../workspace/timeline.svelte";
  import { knowledge, knowledgeAvailable } from "../workspace/knowledge";
  import { knowledgeProviderActive, openAttachSheet } from "../plugins/store";

  interface Props {
    dash: DashCtx;
    sessions: Map<string, Session>;
    names: Map<string, string>;
    wsId: string | null;
    wsRoot: string | null;
    paneId: string;
    ctrl: LayoutCtrl;
    /** False while this retained dashboard tab is behind another pane tab. */
    visible?: boolean;
  }

  let { dash, sessions, names, wsId, wsRoot, paneId, ctrl, visible = true }: Props = $props();

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

  const working = $derived(agents.filter(isBusy).length);
  // Alive-guarded like `working`: a dead row already reads "exited" on its
  // card, so counting its stale "finished" state would make the vital-signs
  // strip disagree with the roster.
  const finished = $derived(
    agents.filter((s) => s.alive && s.agent_state === "finished" && !isBusy(s)).length,
  );
  const busyShells = $derived(shells.filter(isBusy).length);

  const compact = $derived.by(() => {
    const density = getSetting("dashboard.cardDensity");
    return density === "compact" || (density === "auto" && roster.length >= 7);
  });
  const hero = $derived(lane.length === 0 && roster.length === 1 && !compact);

  /** The roster as one line (default) or today's cards — a setting, so the
   *  choice sticks per client and "show cards" / "show line" flip it. */
  const cardsMode = $derived(getSetting("dashboard.roster") === "cards");

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

  // --- the Mastermind ------------------------------------------------------------
  //
  // A bound Mastermind means the workspace is NOT "nothing running": the
  // dashboard chrome shows (an honest empty roster) instead of the launcher
  // blank state. The Mastermind itself lives in the window's panel.
  const blank = $derived(nothingRunning && dash.mastermind === null);

  /** "Continue where you left off": this window's most recent agent, else the
   *  newest live one. Rendered only when it isn't already the whole story. */
  const continueTarget = $derived.by(() => {
    for (const id of dash.mru) {
      const s = sessions.get(id);
      if (
        s !== undefined &&
        s.alive &&
        s.workspace_id === wsId &&
        s.kind === "agent" &&
        // The Mastermind is the observer, never a roster/continue target —
        // the same isMastermind gate every roster surface routes through, so
        // this branch can't leak it in even if it ever lands in the MRU.
        !isMastermind(s)
      )
        return s;
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
   *  against the snapshot's client receipt time) and only while this pane
   *  is shown AND the document is visible (the compound gate — catch-up on
   *  return via the re-run). */
  let computeNow = $state(Date.now());
  $effect(() => {
    const snap = $computeStatus;
    if (snap === null || snap.self === null || !visible || !$pageVisible) return;
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

  // --- "since you left": the viewer's last look ---------------------------------
  //
  // Per viewer, like unread: the baseline is the seq this browser had looked
  // up to when the dashboard was LAST shown, captured once each time it
  // becomes visible (pane tab + document + window focus), and held while it
  // stays visible so rows don't vanish under the reader. While visible the
  // stored mark tracks the head, so the next return counts only newer rows.
  // A stored seq above the daemon's head (data dir wiped) resets to zero.

  let windowFocused = $state(typeof document === "undefined" || document.hasFocus());
  $effect(() => {
    const on = () => (windowFocused = document.hasFocus());
    window.addEventListener("focus", on);
    window.addEventListener("blur", on);
    return () => {
      window.removeEventListener("focus", on);
      window.removeEventListener("blur", on);
    };
  });

  let baseline = $state<SeenMark | null>(null);
  /** The workspace the current baseline was captured for (null = none yet). */
  let baselineFor: string | null = null;
  $effect(() => {
    const shown = visible && $pageVisible;
    const ws = wsId;
    const head = timelineStore.head;
    const settled = timelineStore.available !== null;
    // The read mark advances only while someone is actually looking —
    // visible AND the window focused; a visible-but-unfocused dashboard
    // still captures its baseline so the rows it shows are the right ones.
    const reading = shown && windowFocused;
    if (!shown || ws === null) {
      // Hidden: the next show captures a fresh baseline.
      baselineFor = null;
      return;
    }
    if (!settled) return;
    untrack(() => {
      if (baselineFor !== ws) {
        baseline = lastSeen(ws, head) ?? { seq: 0, ts: 0 };
        baselineFor = ws;
      }
      if (reading && head > 0) markSeen(ws, head);
    });
  });

  /** Minute clock for the "3h 12m" since the last look — ticks only while
   *  someone can see it (pane + document visibility, the compound gate). */
  let now = $state(Date.now());
  $effect(() => {
    if (!visible || !$pageVisible) return;
    now = Date.now();
    const t = setInterval(() => (now = Date.now()), 60_000);
    return () => clearInterval(t);
  });

  const dirtyCount = $derived($gitStatus?.entries.length ?? 0);
</script>

<div class="dashboard">
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
          onclick={() => setMastermindPanelOpen(true)}>+ mastermind</button
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
      <!-- The vital-signs strip: name · the branch chip (git folded in here:
           ahead/behind + uncommitted count, click opens source control) ·
           the summary sentence · the compute chip — one quiet row. -->
      <header class="strip">
        <span class="wsname">{dash.wsName}</span>
        {#if $gitStatus !== null}
          <button
            class="branch"
            title="{$gitStatus.branch ?? 'detached'} · {dirtyCount === 0
              ? 'clean'
              : `${dirtyCount} uncommitted change${dirtyCount === 1 ? '' : 's'}`} — open source control"
            onclick={dash.onOpenGit}
          >
            {$gitStatus.branch ?? ($gitStatus.detached ? "detached" : "—")}
            {#if $gitStatus.ahead > 0}<span class="gnum">↑{$gitStatus.ahead}</span>{/if}
            {#if $gitStatus.behind > 0}<span class="gnum">↓{$gitStatus.behind}</span>{/if}
            {#if dirtyCount > 0}<span class="gnum dirty" aria-label="{dirtyCount} uncommitted changes">●{dirtyCount}</span>{/if}
          </button>
        {/if}
        <span class="sentence">
          {#if working > 0}<b class="w">{working} working</b>{/if}
          {#if lane.length > 0}<b class="a">{lane.length} needs you</b>{/if}
          {#if finished > 0}<b class="d">{finished} finished</b>{/if}
          {#if working === 0 && lane.length === 0 && finished === 0}
            <b class="d">all quiet</b>
          {/if}
        </span>
        {#if computeChip !== null}
          <span class="compute" title={computeChip.title}>{computeChip.text}</span>
        {/if}
        {#if !mastermindPanel.open}
          <!-- The way into the window's Mastermind panel — quiet, and only a
               suggestion: it never opens by itself. -->
          <button
            class="askmm"
            class:setup={dash.mastermind === null}
            title={dash.mastermind !== null
              ? "open the Mastermind panel — it stays with you on every view"
              : "set up a Mastermind: one agent that watches the whole workspace and delegates work"}
            onclick={() => setMastermindPanelOpen(true)}
          >
            <BrandMark size={12} title="Mastermind" />
            <span>{dash.mastermind !== null ? "Ask the Mastermind" : "mastermind"}</span>
            {#if keyHint("mastermind") !== ""}<kbd>{keyHint("mastermind")}</kbd>{/if}
          </button>
        {/if}
      </header>

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
          <!-- Needs you: the attention lane, unchanged. Quiet means quiet. -->
          <section class="needs" aria-labelledby="needs-title">
            <div id="needs-title" class="lbl">Needs you</div>
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
          </section>
        {/if}

        {#if timelineStore.available !== false}
          <!-- Since you left: hidden entirely on a daemon without a Timeline. -->
          <SinceYouLeft {dash} {baseline} {sessions} {names} {wsRoot} {paneId} {ctrl} {now} />
        {/if}

        {#if $knowledgeAvailable !== false}
          <!-- Where things stand: the top of Knowledge, or the one-line
               attach offer. Hidden on a daemon without a knowledge route. -->
          <WhereThingsStand
            knowledge={$knowledge}
            providerActive={$knowledgeProviderActive}
            onOpenKnowledge={dash.onOpenKnowledge}
            onAttach={() => openAttachSheet("mycelium")}
          />
        {/if}

        {#if !nothingRunning}
          {#if cardsMode}
            <!-- Cards: today's roster (the pre-rework surface), on request. -->
            <section class="cards" aria-labelledby="now-title">
              <div class="shead">
                <span id="now-title" class="lbl">Now</span>
                <button class="link" onclick={() => setSetting("dashboard.roster", "line")}>show one line</button>
              </div>
              {#if roster.length > 0}
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
                        {visible}
                      />
                    </div>
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
            </section>
          {:else}
            <NowLine
              {agents}
              {shells}
              {names}
              continueId={continueTarget?.id ?? null}
              onOpenSession={dash.onOpenSession}
              onShowCards={() => setSetting("dashboard.roster", "cards")}
            />
          {/if}
        {/if}
      </div>
        </div>
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
    font-size: var(--text-lg);
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
    font-size: var(--text-xs);
    color: var(--muted);
    border: 1px solid var(--edge);
    border-radius: 3px;
    padding: 0 3px;
  }

  /* --- the populated surface --------------------------------------------------- */
  /* The chrome row: the scrolling surface + the Mastermind dock (or its edge
     pill) as flex siblings — the dock never overlaps content when docked,
     and only the deliberate narrow-width overlay ever floats above it. */
  /* Two fixed columns: only .scroll (and the dock's own transcript) scroll.
     Clip sideways so a too-wide dock child can never pan the whole surface. */
  .body {
    position: absolute;
    inset: 0;
    display: flex;
    min-width: 0;
    overflow-x: clip;
  }
  .scroll {
    flex: 1;
    min-width: 0;
    overflow-y: auto;
  }

  .inner {
    max-width: 1000px;
    min-height: 100%;
    margin: 0 auto;
    padding: 22px 28px 28px;
    display: flex;
    flex-direction: column;
    gap: 22px;
  }

  /* The way into the window's Mastermind panel: a quiet pill at the end of
     the vital-signs strip; "setup" (no Mastermind yet) is quieter still. */
  .askmm {
    flex: none;
    align-self: center;
    appearance: none;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 3px 8px 3px 7px;
    border: 1px solid var(--edge);
    border-radius: 999px;
    background: none;
    color: var(--fg);
    font: inherit;
    font-size: var(--text-xs);
    cursor: pointer;
    transition:
      border-color 0.12s ease,
      background-color 0.12s ease;
  }
  .askmm:hover {
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
    background: color-mix(in srgb, var(--accent) 8%, transparent);
  }
  .askmm.setup {
    border-color: transparent;
    color: var(--muted);
  }
  .askmm kbd {
    font-family: var(--mono);
    font-size: 10px;
    color: var(--muted);
    border: 1px solid var(--edge);
    border-radius: 4px;
    padding: 0 4px;
    line-height: 1.5;
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
  /* The branch chip is git's whole presence here: branch, ahead/behind, and
     the uncommitted count, opening source control on click. */
  .branch {
    appearance: none;
    background: none;
    cursor: pointer;
    font: inherit;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--git-modified);
    border: 1px solid color-mix(in srgb, var(--git-modified) 35%, transparent);
    border-radius: 999px;
    padding: 1px 8px;
    max-width: 260px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    display: inline-flex;
    align-items: baseline;
    gap: 6px;
    transition: border-color 0.12s ease;
  }
  .branch:hover {
    border-color: var(--git-modified);
  }
  .gnum {
    color: var(--muted);
  }
  .gnum.dirty {
    font-size: 10px;
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

  /* One column of question-shaped sections; the Now line pins to the
     bottom when the surface is taller than its content. */
  .main {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 22px;
    min-width: 0;
  }
  .main > :global(.now) {
    margin-top: auto;
  }

  /* Small-caps section labels — the one label voice for every section
     (the blank state's quieter lowercase title stays .sec-title). */
  .lbl {
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
    font-weight: 600;
  }
  .sec-title {
    font-size: var(--text-xs);
    color: var(--muted);
    letter-spacing: 0.04em;
    text-transform: lowercase;
    padding: 4px 2px 2px;
  }
  .shead {
    display: flex;
    align-items: baseline;
    gap: 10px;
  }
  .link {
    margin-left: auto;
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--accent);
    cursor: pointer;
    white-space: nowrap;
  }
  .link:hover {
    text-decoration: underline;
  }

  .needs,
  .cards {
    display: flex;
    flex-direction: column;
    gap: 10px;
    min-width: 0;
  }

  .lane {
    display: flex;
    flex-direction: column;
    gap: 8px;
    border-left: 2px solid color-mix(in srgb, var(--warn) 55%, transparent);
    padding-left: 10px;
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

  /* --- blank-state recents ---------------------------------------------------- */
  .rrow {
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
  .rrow:hover {
    background: var(--row-hover);
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
</style>
