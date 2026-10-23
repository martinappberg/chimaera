<script lang="ts">
  import { SvelteMap } from "svelte/reactivity";
  import CountdownRing from "../shared/CountdownRing.svelte";
  import GoalGlyph from "../shared/GoalGlyph.svelte";
  import Chevron from "../shared/Chevron.svelte";
  import StopNote from "./StopNote.svelte";
  import { clickVia } from "./stopConfirm";
  import { liveClock } from "../shared/liveClock.svelte";
  import {
    compactCount,
    goalStatusWord,
    jobPeriodMs,
    ringFraction,
    shortDuration,
    untilLabel,
    type GoalInfo,
    type ScheduledJob,
    type ScheduledWakeup,
  } from "./ahead";

  /**
   * What the agent will do on its own: its goal, its pending wakeup (a
   * self-paced `/loop`), its interval jobs. A pinned card in the WorkTray
   * family, one line per thing, in the agent's own words, with a still ring
   * and a time — nothing moves while it waits. Actions sit in the time's
   * cell and swap in on hover or focus, so a row never changes size; a click
   * opens the row's detail, where the same actions are text (the touch
   * path). Three or more scheduled things fold into one line. Inside the
   * folded work strip it draws one part without its own chrome.
   */
  interface Props {
    wakeup: ScheduledWakeup | null;
    jobs: ScheduledJob[];
    /** A goal still in play (the caller drops a finished one). */
    goal: GoalInfo | null;
    /** A turn is running — a due wakeup or job is the work in progress. */
    running: boolean;
    visible?: boolean;
    /** Everything (the card), or one part inside the folded strip. */
    part?: "all" | "goal" | "schedule";
    /** Rows only: the strip draws the chrome. */
    embedded?: boolean;
    /** Ask the agent to stop its loop (Claude has no control for it). */
    onStopLoop?: () => void;
    /** Ask the agent to cancel one job. */
    onCancelJob?: (id: string) => void;
    onGoalPause?: () => void;
    onGoalResume?: () => void;
    /** Clear the goal, pressed by keyboard or pointer (the host may ask
     *  first: claude clears a running goal by stopping the turn). */
    onGoalClear?: (via: "key" | "pointer") => void;
    /** A Clear waiting for its second press: what else it ends, and the
     *  hint. Shown above the Clear that was pressed. */
    goalConfirm?: { text: string; how: string } | null;
  }
  let {
    wakeup,
    jobs,
    goal,
    running,
    visible = true,
    part = "all",
    embedded = false,
    onStopLoop,
    onCancelJob,
    onGoalPause,
    onGoalResume,
    onGoalClear,
    goalConfirm = null,
  }: Props = $props();

  /** Which Clear asked (the row's hover action or the detail's link): its
   *  note sits above that one. */
  let clearFrom = $state<"row" | "detail">("row");
  function clearGoal(from: "row" | "detail", e: MouseEvent) {
    clearFrom = from;
    onGoalClear?.(clickVia(e));
  }

  type Row =
    | { key: "wakeup"; kind: "wakeup"; at: number; from: number; text: string; wakeup: ScheduledWakeup }
    | { key: string; kind: "job"; at: number | null; from: number; text: string; job: ScheduledJob };

  const scheduled = $derived.by<Row[]>(() => {
    const rows: Row[] = [];
    if (wakeup !== null) {
      rows.push({
        key: "wakeup",
        kind: "wakeup",
        at: wakeup.atMs,
        from: wakeup.scheduledAtMs,
        text: wakeup.reason || wakeup.prompt || "Next wakeup",
        wakeup,
      });
    }
    for (const job of jobs) {
      rows.push({
        key: `job:${job.id}`,
        kind: "job",
        at: job.nextAtMs,
        from: job.nextAtMs !== null ? job.nextAtMs - jobPeriodMs(job.cron) : 0,
        text: job.prompt,
        job,
      });
    }
    // Soonest first; an unknown next run last.
    return rows.sort((a, b) => (a.at ?? Infinity) - (b.at ?? Infinity));
  });
  const showGoal = $derived(part !== "schedule" && goal !== null);
  const showSchedule = $derived(part !== "goal" && scheduled.length > 0);
  /** Three or more scheduled things are one line until opened. */
  const folds = $derived(scheduled.length >= 3);
  let listOpen = $state(false);
  /** Which rows show their detail. */
  let opened = $state<string | null>(null);

  /** One shared 1 Hz clock while anything here counts down and is seen. */
  const clock = liveClock(() => visible && showSchedule);
  const now = $derived(showSchedule ? clock.ms : 0);

  /** Rows the user asked to stop, until the agent's own schedule drops them
   *  (or three minutes pass — the agent may decline). */
  const stopping = new SvelteMap<string, number>();
  const stoppingKeys = $derived.by(() => {
    const live = new Set(scheduled.map((r) => r.key));
    // Read through the clock, so a request the agent ignored lapses.
    const t = now || Date.now();
    const keys = new Set<string>();
    for (const [key, at] of stopping) {
      if (live.has(key) && t - at < 180_000) keys.add(key);
    }
    return keys;
  });
  function stop(row: Row) {
    if (row.kind === "wakeup") onStopLoop?.();
    else onCancelJob?.(row.job.id);
    stopping.set(row.key, Date.now());
  }
  const canStop = (row: Row) =>
    row.kind === "wakeup" ? onStopLoop !== undefined : onCancelJob !== undefined;

  function when(row: Row): string {
    if (stoppingKeys.has(row.key)) return "stopping…";
    return untilLabel(row.at, now);
  }
  const due = (row: Row) => row.at !== null && row.at <= now;

  const goalWord = $derived(goal !== null ? goalStatusWord(goal.status) : null);
  const goalTone = $derived<"active" | "idle" | "warn">(
    goal === null || goal.status === "active" ? "active" : goal.status === "paused" ? "idle" : "warn",
  );
  const goalMeta = $derived.by(() => {
    if (goal === null) return "";
    if (goal.tokenBudget !== null && goal.tokensUsed !== null) {
      return `${compactCount(goal.tokensUsed)} / ${compactCount(goal.tokenBudget)}`;
    }
    // A fresh goal has spent nothing yet: say nothing rather than "0 tokens".
    if (goal.tokensUsed !== null && goal.tokensUsed > 0) return `${compactCount(goal.tokensUsed)} tokens`;
    if (goal.turns !== null && goal.turns > 0) return `${goal.turns} ${goal.turns === 1 ? "turn" : "turns"}`;
    return "";
  });
  const budgetShare = $derived(
    goal !== null && goal.tokenBudget !== null && goal.tokenBudget > 0 && goal.tokensUsed !== null
      ? Math.min(1, goal.tokensUsed / goal.tokenBudget)
      : null,
  );
  const goalHasActions = $derived(
    (goal?.status === "active" && onGoalPause !== undefined) ||
      (goal?.status !== "active" && onGoalResume !== undefined) ||
      onGoalClear !== undefined,
  );

  /** Inside the folded strip the line above already reads the goal, so its
   *  part opens straight to the detail. */
  const goalInline = $derived(part === "goal" && embedded);
  const rowAsks = $derived(goalConfirm !== null && clearFrom === "row" && !goalInline);
  const detailAsks = $derived(goalConfirm !== null && (clearFrom === "detail" || goalInline));

  function toggle(key: string) {
    opened = opened === key ? null : key;
  }
  const ago = (ms: number) => (ms > 0 ? `${shortDuration(Math.max(1, (Date.now() - ms) / 1000))} ago` : "");
  const clockTime = (ms: number) => new Date(ms).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  const expires = (job: ScheduledJob) =>
    job.recurring && job.createdAtMs > 0
      ? new Date(job.createdAtMs + 7 * 86_400_000).toLocaleDateString([], { month: "short", day: "numeric" })
      : null;
</script>

{#snippet scheduleRow(row: Row)}
  <div class="row" class:stopping={stoppingKeys.has(row.key)}>
    <button
      type="button"
      class="main"
      aria-expanded={opened === row.key}
      onclick={() => toggle(row.key)}
      title={row.kind === "job" && row.job.schedule !== "" ? row.job.schedule : undefined}
    >
      <span class="glyph">
        {#if due(row) && running}
          <span class="now-dot" class:still={!visible}></span>
        {:else}
          <CountdownRing fraction={ringFraction(row.at, row.from, now)} soon={row.at !== null && row.at - now < 60_000} />
        {/if}
      </span>
      <span class="kind">{row.kind === "wakeup" ? "Loop" : "Scheduled"}</span>
      <span class="text"
        >{#if row.kind === "job" && row.job.schedule !== ""}<span class="sched">{row.job.schedule}</span>{" · "}{/if}{row.text}</span
      >
    </button>
    <span class="trail">
      <span class="meta">{when(row)}</span>
      {#if canStop(row) && !stoppingKeys.has(row.key)}
        <span class="acts">
          <button type="button" class="act stop" onclick={() => stop(row)} title={row.kind === "wakeup" ? "Asks the agent to stop the loop" : "Asks the agent to cancel this job"}
            >{row.kind === "wakeup" ? "Stop" : "Cancel"}</button
          >
        </span>
      {/if}
    </span>
  </div>
  {#if opened === row.key}
    <div class="detail">
      {#if row.kind === "wakeup"}
        <span class="k">wakes</span><span class="v">{clockTime(row.wakeup.atMs)}</span>
        {#if row.wakeup.prompt !== ""}<span class="k">sends</span><span class="v">“{row.wakeup.prompt}”</span>{/if}
      {:else}
        {#if row.job.schedule !== ""}<span class="k">runs</span><span class="v">{row.job.schedule}</span>{/if}
        <span class="k">sends</span><span class="v">“{row.job.prompt}”</span>
        {#if row.at !== null}<span class="k">next</span><span class="v">{clockTime(row.at)}</span>{/if}
        {#if expires(row.job) !== null}<span class="k">ends</span><span class="v">{expires(row.job)}</span>{/if}
      {/if}
      {#if canStop(row) && !stoppingKeys.has(row.key)}
        <span class="k"></span><span class="v"
          ><button type="button" class="link" onclick={() => stop(row)}
            >{row.kind === "wakeup" ? "Stop the loop" : "Cancel this job"}</button
          ></span
        >
      {/if}
    </div>
  {/if}
{/snippet}

<div class="ahead" class:pinned-card={!embedded} class:visible class:embedded>
  {#if showGoal && goal !== null}
    {#if !goalInline}
    <div class="row goal-row" class:armed={rowAsks}>
      {#if rowAsks && goalConfirm !== null}
        <StopNote text={goalConfirm.text} how={goalConfirm.how} control="goal" onConfirm={() => onGoalClear?.("pointer")} />
      {/if}
      <button type="button" class="main" aria-expanded={opened === "goal"} onclick={() => toggle("goal")}>
        <span class="glyph"><GoalGlyph tone={goalTone} /></span>
        <span class="kind">Goal</span>
        <span class="text goal-text">{goal.objective}</span>
      </button>
      <span class="trail">
        <span class="meta">
          {#if budgetShare !== null}<span class="bar" aria-hidden="true"><i style="width:{budgetShare * 100}%"></i></span>{/if}
          {#if goalWord !== null}<span class="word" class:warn={goalTone === "warn"}>{goalWord}</span>{#if goalMeta !== ""}<span class="sep">·</span>{/if}{/if}
          {goalMeta}
        </span>
        {#if goalHasActions}
          <span class="acts">
            {#if goal.status === "active" && onGoalPause !== undefined}
              <button type="button" class="act" onclick={onGoalPause} title="/goal pause">Pause</button>
            {:else if goal.status !== "active" && onGoalResume !== undefined}
              <button type="button" class="act" onclick={onGoalResume} title="/goal resume">Resume</button>
            {/if}
            {#if onGoalClear !== undefined}
              <button
                type="button"
                class="act stop"
                class:armed={rowAsks}
                data-stop-control="goal"
                onclick={(e) => clearGoal("row", e)}
                title="/goal clear">Clear</button
              >
            {/if}
          </span>
        {/if}
      </span>
    </div>
    {/if}
    {#if opened === "goal" || goalInline}
      <div class="detail" class:inline={goalInline}>
        <span class="k">goal</span><span class="v">{goal.objective}</span>
        {#if goalWord !== null}<span class="k">status</span><span class="v" class:warn-text={goalTone === "warn"}>{goalWord}</span>{/if}
        {#if goal.turns !== null && goal.turns > 0}<span class="k">checked</span><span class="v">{goal.turns} {goal.turns === 1 ? "turn" : "turns"}</span>{/if}
        {#if goal.lastCheck !== null}<span class="k">last check</span><span class="v">{goal.lastCheck}</span>{/if}
        {#if goal.timeUsedSecs !== null && goal.timeUsedSecs > 0}<span class="k">time</span><span class="v">{shortDuration(goal.timeUsedSecs)}</span>{/if}
        {#if goal.tokensUsed !== null}<span class="k">tokens</span><span class="v"
            >{compactCount(goal.tokensUsed)}{goal.tokenBudget !== null ? ` of ${compactCount(goal.tokenBudget)}` : ""}</span
          >{/if}
        {#if goal.setAtMs > 0}<span class="k">set</span><span class="v">{ago(goal.setAtMs)}</span>{/if}
        {#if goalHasActions}
          <span class="k"></span><span class="v links">
            {#if goal.status === "active" && onGoalPause !== undefined}<button type="button" class="link" onclick={onGoalPause}>Pause</button>{/if}
            {#if goal.status !== "active" && onGoalResume !== undefined}<button type="button" class="link" onclick={onGoalResume}>Resume</button>{/if}
            {#if onGoalClear !== undefined}<span class="clear-anchor"
                >{#if detailAsks && goalConfirm !== null}<StopNote
                    text={goalConfirm.text}
                    how={goalConfirm.how}
                    control="goal"
                    onConfirm={() => onGoalClear?.("pointer")}
                  />{/if}<button
                  type="button"
                  class="link"
                  class:armed={detailAsks}
                  data-stop-control="goal"
                  onclick={(e) => clearGoal("detail", e)}>Clear the goal</button
                ></span
              >{/if}
          </span>
        {/if}
      </div>
    {/if}
  {/if}

  {#if showSchedule}
    {#if folds}
      {@const first = scheduled[0]}
      <div class="row">
        <button type="button" class="main" aria-expanded={listOpen} onclick={() => (listOpen = !listOpen)}>
          <span class="glyph">
            <CountdownRing fraction={ringFraction(first.at, first.from, now)} soon={first.at !== null && first.at - now < 60_000} />
          </span>
          <span class="text">{scheduled.length} scheduled · next: {first.kind === "job" && first.job.schedule !== "" ? first.job.schedule : first.text}</span>
        </button>
        <span class="trail"><span class="meta">{untilLabel(first.at, now)}</span></span>
        <span class="fold-chev"><Chevron open={listOpen} /></span>
      </div>
      {#if listOpen}
        <div class="nested">
          {#each scheduled as row (row.key)}
            {@render scheduleRow(row)}
          {/each}
        </div>
      {/if}
    {:else}
      {#each scheduled as row (row.key)}
        {@render scheduleRow(row)}
      {/each}
    {/if}
  {/if}
</div>

<style>
  /* Chrome is the shared .pinned-card recipe; the host sets width and the
     stacking margins. Padding matches the trays so the family lines up. */
  .ahead {
    padding: 4px 12px 5px 11px;
    font-size: var(--text-sm);
    line-height: 18px;
    color: var(--muted);
  }
  .ahead.embedded {
    padding: 0;
  }
  /* A row is exactly one line, hover or not: the time and the actions share
     one grid cell and swap visibility, so nothing reflows under the pointer. */
  .row {
    display: flex;
    align-items: center;
    gap: 6px;
    height: 22px;
    min-width: 0;
  }
  .main {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 9px;
    height: 100%;
    padding: 0;
    border: none;
    background: none;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .row:hover,
  .row:focus-within {
    color: var(--fg);
  }
  .glyph {
    flex: none;
    width: 14px;
    height: 14px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
  }
  .text {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .kind {
    flex: none;
    font-size: var(--text-xs);
    font-weight: 500;
    color: var(--muted);
  }
  .goal-text {
    color: color-mix(in srgb, var(--muted) 45%, var(--fg));
  }
  .row:hover .goal-text,
  .row:focus-within .goal-text {
    color: var(--fg);
  }
  .sched {
    color: color-mix(in srgb, var(--muted) 45%, var(--fg));
  }
  .trail {
    flex: none;
    display: grid;
    justify-items: end;
    align-items: center;
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }
  .trail > * {
    grid-area: 1 / 1;
  }
  .meta {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    white-space: nowrap;
  }
  .acts {
    display: inline-flex;
    gap: 2px;
    visibility: hidden;
  }
  .row:hover .acts,
  .row:focus-within .acts {
    visibility: visible;
  }
  .row:hover .meta:has(~ .acts),
  .row:focus-within .meta:has(~ .acts) {
    visibility: hidden;
  }
  .act {
    height: 18px;
    padding: 0 6px;
    border: none;
    border-radius: 4px;
    background: none;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-xs);
    line-height: 18px;
    cursor: pointer;
    transition: color 0.12s ease, background-color 0.12s ease;
  }
  .act:hover,
  .act:focus-visible {
    color: var(--fg);
    background: var(--hover-tint);
  }
  .act.stop:hover,
  .act.stop:focus-visible {
    color: var(--err);
  }
  /* A Clear waiting for its second press: the note above names what else
     it stops; the Clear stays shown (hover or not) in the warning tone. */
  .goal-row {
    position: relative;
    --stop-note-max: 100%;
  }
  .row.armed .acts {
    visibility: visible;
  }
  .row.armed .meta:has(~ .acts) {
    visibility: hidden;
  }
  .act.armed,
  .act.armed:hover,
  .act.armed:focus-visible {
    color: var(--warn);
    background: color-mix(in srgb, var(--warn) 12%, transparent);
  }
  .clear-anchor {
    position: relative;
    --stop-note-right: auto;
    --stop-note-left: 0;
  }
  .link.armed {
    color: var(--warn);
    text-decoration-color: color-mix(in srgb, var(--warn) 50%, transparent);
  }
  .stopping .text {
    text-decoration: line-through;
    text-decoration-color: color-mix(in srgb, var(--muted) 60%, transparent);
  }
  .stopping {
    opacity: 0.65;
  }
  .word.warn {
    color: var(--warn);
  }
  .sep {
    opacity: 0.7;
  }
  .bar {
    width: 30px;
    height: 3px;
    border-radius: 2px;
    background: color-mix(in srgb, var(--muted) 30%, transparent);
    overflow: hidden;
    display: inline-block;
  }
  .bar > i {
    display: block;
    height: 100%;
    background: var(--muted);
    border-radius: 2px;
  }
  .fold-chev {
    flex: none;
    display: inline-flex;
  }
  .nested {
    padding-left: 23px;
  }
  .detail {
    display: grid;
    grid-template-columns: max-content minmax(0, 1fr);
    gap: 1px 10px;
    padding: 1px 0 6px 23px;
    font-size: var(--text-xs);
    line-height: 1.45;
  }
  .detail.inline {
    padding: 2px 0 4px 6px;
  }
  .warn-text {
    color: var(--warn);
  }
  .detail .k {
    color: var(--muted);
    opacity: 0.85;
  }
  .detail .v {
    color: var(--fg);
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .links {
    display: flex;
    gap: 12px;
  }
  .link {
    padding: 0;
    border: none;
    background: none;
    font: inherit;
    color: var(--muted);
    text-decoration: underline;
    text-decoration-color: color-mix(in srgb, var(--muted) 40%, transparent);
    text-underline-offset: 3px;
    cursor: pointer;
  }
  .link:hover,
  .link:focus-visible {
    color: var(--fg);
  }
  .now-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--accent);
    animation: tray-breathe 1.8s ease-in-out infinite;
  }
  .now-dot.still {
    animation: none;
  }
  :global(html.app-hidden) .now-dot {
    animation-play-state: paused;
  }
  @media (prefers-reduced-motion: reduce) {
    .now-dot {
      animation: none;
    }
  }
  /* Touch screens have no hover: the row's detail (a tap) carries the
     actions as text, so the time never gives way to them. */
  @media (hover: none) {
    .acts {
      display: none;
    }
  }
</style>
