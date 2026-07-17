<script lang="ts">
  /**
   * One roster card on the workspace dashboard: who, doing what, produced
   * what — and a door into the live session (the whole card opens it).
   * Renders from server wire truth alone; the v0.2 status-feed fields let a
   * hooks-tier claude TUI card carry a now-line, a read-only work drop-down
   * (wire subagents), the context meter + cost (statusline usage), and a
   * stalled warning. When a warm chat store is passed (bounded upstream in
   * DashboardView) it upgrades to the richer store-derived now-line, plan
   * snapshot, and work drop-down (live subagents ∪ background tasks with
   * stop controls) — never both sources at once (double counting). Zones a
   * card can't truthfully fill stay empty — never fabricated.
   */
  import SessionGlyph from "../shared/SessionGlyph.svelte";
  import WorkTray from "../shared/WorkTray.svelte";
  import WorkTrayRow from "../shared/WorkTrayRow.svelte";
  import { basename } from "../previews/files";
  import { formatElapsedSeconds } from "../shared/time";
  import { relativeAge } from "../workspace/launcher";
  import { agentKind, dotState, dotTitle, type Session } from "../workspace/sessions";
  import { isUnread } from "../workspace/unread.svelte";
  import type { BackgroundTask, ChatBlock, ChatStore } from "../chat/store.svelte";
  import { provenanceOf, provenanceTitle , relPath as sharedRelPath} from "./dash";

  interface Props {
    session: Session;
    name: string;
    /** Warm chat store for rich detail; null = wire-truth only. */
    store: ChatStore | null;
    /** Compact row rendering (the 7+ agents triage density). */
    compact?: boolean;
    /** Single-agent hero: plan snapshot + subagents open by default. */
    hero?: boolean;
    wsRoot: string | null;
    onOpen: () => void;
    onOpenChanges: () => void;
    /** Stop a work row — a subagent or a background task; both ride the
     *  same stop_task wire (claude chat only). Omitted when unsupported. */
    onStopTask?: (id: string) => void;
  }

  let {
    session,
    name,
    store,
    compact = false,
    hero = false,
    wsRoot,
    onOpen,
    onOpenChanges,
    onStopTask,
  }: Props = $props();

  const prov = $derived(provenanceOf(session));
  const touched = $derived(session.files_touched ?? []);

  /** One honest line about what the session is doing right now. */
  const nowLine = $derived.by(() => {
    // Dead first: a SIGKILLed TUI fires no clearing hook, so the record can
    // carry a stale present-tense now_line until the watcher retires the
    // row — "exited" must win over every activity line.
    if (!session.alive) return "exited";
    if (store !== null) {
      const act = store.activity;
      if (act !== null && act.detail !== "") return act.detail;
      if (act !== null) return act.kind;
      const step = store.plan.find((p) => p.status === "in_progress");
      if (step !== undefined) return step.content;
      const lastMsg = store.blocks.findLast((b) => b.kind === "message");
      if (lastMsg !== undefined) return lastMsg.text.slice(0, 160);
    }
    // The wire now_line (a claude TUI's latest-hook summary) beats the
    // files_touched fallback — fresher and more specific ("ran Bash" vs the
    // last write).
    if (session.now_line != null && session.now_line !== "") return session.now_line;
    // The agent's own post-turn status line (SessionStatus): "where things
    // stand" for a chat row with no warm store — the same line the rail's
    // second row wears.
    if (session.status_detail != null && session.status_detail !== "") {
      return session.status_detail;
    }
    // Hook-level fallback: the last file the agent wrote.
    if (prov === "hooks" && touched.length > 0) {
      return `edited ${basename(touched[touched.length - 1])}`;
    }
    if (prov === "none") {
      // Output recency is the only honest signal for unintegrated TUIs
      // (dotTitle's working/quiet vocabulary); absent = an old daemon, so
      // the state really is unknown.
      if (session.output_active === true) return "working — terminal output flowing";
      if (session.output_active === false) return "quiet — no recent output";
      return "state unknown — open the terminal";
    }
    return null;
  });

  /** Subagents in flight (the AgentsTray derivation, promoted card-side). */
  const activeAgents = $derived(
    store === null
      ? []
      : store.blocks.filter(
          (b): b is Extract<ChatBlock, { kind: "tool" }> =>
            b.kind === "tool" && b.tool === "agent" && b.status === "in_progress",
        ),
  );

  /** Wire subagents (claude TUI SubagentStart hooks) — the read-only tier
   *  for cards with no warm store. Never unioned with the store rows: a
   *  warm store derives the SAME work from its journal, so merging the two
   *  sources would double-count it. */
  const wireAgents = $derived(store === null ? (session.subagents ?? []) : []);

  /** Background work (backgrounded Bash / workflows) — the level-set the
   *  BackgroundTray renders, promoted card-side the same way. A card that
   *  showed only subagents would lie by omission (design decision 12). */
  const bgTasks = $derived(store?.backgroundTasks ?? []);
  /** Exactly one subagent source is populated (see wireAgents), so the sum
   *  is the count — the summary label reads the same either way. */
  const subCount = $derived(activeAgents.length + wireAgents.length);
  const workCount = $derived(subCount + bgTasks.length);

  /** "2 subagents · 1 background task" — a zero half is omitted. */
  const workLabel = $derived(
    [
      subCount > 0 ? `${subCount} subagent${subCount === 1 ? "" : "s"}` : null,
      bgTasks.length > 0
        ? `${bgTasks.length} background task${bgTasks.length === 1 ? "" : "s"}`
        : null,
    ]
      .filter((p) => p !== null)
      .join(" · "),
  );
  /** The trays' own identity glyphs: ✳ subagents, ⧖ background-only. */
  const workGlyph = $derived(subCount > 0 ? "✳" : "⧖");

  /** The drop-down: closed by default so ten cards stay scannable; the
   *  single-agent hero opens it. Writable derived: the tray's own toggle
   *  (bound below) overrides until the hero default itself changes. */
  let workOpen = $derived(hero);

  /** 1 Hz clock for the rows' elapsed/age columns (background elapsed, wire
   *  subagent age) — only while the rows are actually visible (the
   *  BackgroundTray idiom: collapsed cards must not tick a wake-up per
   *  second for hours). */
  let now = $state(Date.now());
  $effect(() => {
    if (!workOpen || (bgTasks.length === 0 && wireAgents.length === 0)) return;
    now = Date.now();
    const timer = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(timer);
  });

  /** Elapsed since the driver first saw the task. Daemon-side epoch ms, so
   *  clamp: a skewed client clock must not render "-3s". */
  function elapsed(t: BackgroundTask): string {
    if (t.startedAtMs <= 0) return "";
    return formatElapsedSeconds(Math.max(0, Math.floor((now - t.startedAtMs) / 1000)));
  }

  /** Driver titles are "Agent: {description}" — the prefix is the tray's. */
  function subName(title: string): string {
    return title.startsWith("Agent: ") ? title.slice(7) : title;
  }
  function subProgress(b: Extract<ChatBlock, { kind: "tool" }>): string {
    return b.content?.kind === "output" ? (b.content.text ?? "").trim() : "";
  }

  const relPath = (p: string): string => sharedRelPath(wsRoot, p);

  const planEntries = $derived(hero && store !== null ? store.plan.slice(0, 6) : []);
  /** Context meter: the warm store's live figure first, else the claude-TUI
   *  statusline heartbeat — same meter, same thresholds, only the source
   *  differs (chat rows carry wire usage null, so the two never overlap). */
  const ctxPct = $derived(store?.contextPct ?? session.usage?.context_pct ?? null);
  /** The meter tooltip names the model only when the statusline heartbeat
   *  carries one (the store tier doesn't track it — don't fabricate). */
  const ctxTitle = $derived(
    store === null && session.usage?.model != null
      ? `context window used — ${session.usage.model}`
      : "context window used",
  );
  /** Session cost from the statusline heartbeat (claude TUI only; chat rows
   *  carry wire usage null — their cost story is a later pass). */
  const costUsd = $derived(session.usage?.cost_usd ?? null);

  /** The hooks-tier stall check: the record claims running but the PTY has
   *  been silent past the daemon's window, so the claim is likely stale —
   *  say so instead of a stale now-line (honest status). */
  const isStalled = $derived(session.stalled === true);

  function onKeydown(e: KeyboardEvent): void {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      onOpen();
    }
  }
</script>

<!-- The card is the door: click anywhere opens the live session. Inner
     controls (stop, view changes, the drop-down) stop propagation. -->
<div
  class="card"
  class:compact
  class:dead={!session.alive}
  role="button"
  tabindex="0"
  onclick={onOpen}
  onkeydown={onKeydown}
>
  <div class="top">
    <SessionGlyph
      kind={session.kind}
      agentKind={session.agent_kind}
      state={dotState(session)}
      size={12}
      title={dotTitle(session)}
    />
    <span class="dot {dotState(session)}" title={dotTitle(session)}></span>
    <span class="name" title={name}>{name}</span>
    {#if isUnread(session.id)}
      <!-- Finished with output the user hasn't seen — the rail's faint mark,
           worn card-side too (cleared the moment the session is focused). -->
      <span class="unread-dot" title="finished — output you haven't looked at"></span>
    {/if}
    {#if compact && isStalled}
      <!-- Compact rows carry no now-line, but the stall warning is built for
           exactly this density (a wedged agent in a fan-out must not look
           identical to a working one). -->
      <span
        class="stall-mark"
        title="the agent reports running but its terminal has been silent — it may be stuck"
        >stalled</span
      >
    {/if}
    <!-- The kind chip carries the provenance story in its tooltip; a chat
         row IS the authoritative tier ("chat" already says so), so only the
         degraded tiers wear an extra chip — in words a user can read, not
         the old "protocol" jargon. -->
    <span class="chip" title={provenanceTitle(prov)}
      >{agentKind(session)} · {session.ui === "chat" ? "chat" : "term"}</span
    >
    {#if prov !== "protocol"}
      <span class="prov {prov}" title={provenanceTitle(prov)}>
        {prov === "hooks" ? "status via hooks" : "output only"}
      </span>
    {/if}
  </div>

  {#if !compact && isStalled}
    <!-- Stalled overrides the now-line: whatever the hooks last said is
         exactly the claim the silence contradicts. -->
    <div
      class="now stalled"
      title="the agent reports running but its terminal has been silent — it may be stuck"
    >
      stalled — terminal output has gone quiet
    </div>
  {:else if !compact && nowLine !== null}
    <div class="now" title={nowLine}>{nowLine}</div>
  {/if}

  {#if planEntries.length > 0}
    <div class="plan">
      {#each planEntries as p, i (i)}
        <div class="plan-row {p.status}">
          <span class="plan-mark" aria-hidden="true"></span>
          <span class="plan-text">{p.content}</span>
        </div>
      {/each}
    </div>
  {/if}

  {#if !compact && workCount > 0}
    <!-- The tray owns its clicks (expand, stop) — none may bubble into the
         card's open-the-session handler. -->
    <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
    <div class="work" onclick={(e) => e.stopPropagation()}>
      <WorkTray glyph={workGlyph} bind:open={workOpen} label={workLabel}>
        {#each activeAgents as a (a.id)}
          <WorkTrayRow
            onStop={onStopTask !== undefined ? () => onStopTask(a.id) : undefined}
            stopTitle="stop this subagent"
          >
            <span class="subname" title={subName(a.title)}>{subName(a.title)}</span>
            {#if subProgress(a) !== ""}
              <span class="subprog" title={subProgress(a)}>{subProgress(a)}</span>
            {/if}
          </WorkTrayRow>
        {/each}
        {#each wireAgents as a (a.id)}
          <!-- Hook-tier rows are read-only: hooks can't stop a TUI subagent,
               and a stop button that can't work would be a lie. The label is
               the agent's own type name — canonical, never relabeled. -->
          <WorkTrayRow>
            <span class="subname" title={a.label}>{a.label}</span>
            <span class="subage">{relativeAge(Math.floor(a.started_at / 1000), now)}</span>
          </WorkTrayRow>
        {/each}
        {#each bgTasks as t (t.id)}
          {@const e = elapsed(t)}
          <WorkTrayRow
            onStop={onStopTask !== undefined ? () => onStopTask(t.id) : undefined}
            stopTitle="stop this background task"
          >
            <!-- The lane name (local_bash, …) stays canonical in the tooltip;
                 workflow lanes wear their meta.name + agent tally (the
                 BackgroundTray's rich-row facts, card-density). -->
            <span class="bgname" title={t.taskType}
              >{t.workflowName !== null && t.workflowName !== ""
                ? t.workflowName
                : t.description}</span
            >
            {#if t.agentsTotal > 0}
              <span class="bgagents">{t.agentsDone}/{t.agentsTotal} agents</span>
            {/if}
            {#if t.status !== "running"}
              <span class="bgstatus">{t.status}</span>
            {/if}
            {#if e !== ""}
              <span class="bgelapsed">{e}</span>
            {/if}
          </WorkTrayRow>
        {/each}
      </WorkTray>
    </div>
  {/if}

  <div class="meta">
    {#if ctxPct !== null}
      <span class="ctxwrap" title={ctxTitle}>
        <span class="ctxlabel">ctx</span>
        <span class="ctx" class:hot={ctxPct > 80}><i style:width="{Math.min(ctxPct, 100)}%"></i></span>
        <span class="ctxpct">{Math.round(ctxPct)}%</span>
      </span>
    {/if}
    {#if touched.length > 0}
      <button
        class="evidence"
        title={touched.map(relPath).join("\n")}
        onclick={(e) => {
          e.stopPropagation();
          onOpenChanges();
        }}
      >
        {touched.length} file{touched.length === 1 ? "" : "s"} · view changes
      </button>
    {/if}
    <span class="age">{relativeAge(session.created_at)}</span>
    {#if costUsd !== null}
      <span class="cost" title="session cost — from the claude statusline">
        ${costUsd.toFixed(2)}
      </span>
    {/if}
  </div>
</div>

<style>
  .card {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
    padding: 10px 12px;
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 8px;
    text-align: left;
    cursor: pointer;
    transition: border-color 0.12s ease;
    animation: rise 0.18s ease;
  }
  @media (prefers-reduced-motion: reduce) {
    .card {
      animation: none;
    }
  }
  .card:hover {
    border-color: color-mix(in srgb, var(--accent) 45%, var(--edge));
  }
  .card.dead {
    opacity: 0.75;
  }
  .card.compact {
    flex-direction: row;
    align-items: center;
    gap: 8px;
    padding: 6px 10px;
  }

  .top {
    display: flex;
    align-items: center;
    gap: 7px;
    min-width: 0;
    flex-wrap: wrap;
    row-gap: 4px;
  }
  .compact .top {
    flex: 1;
    flex-wrap: nowrap;
  }

  .name {
    flex: 1;
    min-width: 90px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono);
    font-size: var(--text-md);
    color: var(--fg);
  }

  .chip {
    flex: none;
    font-family: var(--mono);
    font-size: 10px;
    color: var(--muted);
    border: 1px solid var(--edge);
    border-radius: 999px;
    padding: 0 6px;
  }

  /* The honesty axis: which tier this card's status truthfully comes from.
     Worn only by the degraded tiers — a chat row's kind chip already says
     it's the authoritative one (tooltip carries the words). */
  .prov {
    flex: none;
    font-family: var(--mono);
    font-size: 10px;
    cursor: help;
    opacity: 0.85;
  }
  .prov.hooks {
    color: var(--muted);
  }
  .prov.none {
    color: var(--warn);
  }

  /* Finished output the user hasn't seen: the rail's faint fg-toned mark —
     never a state color (the dot owns state). Same geometry + fade-in as the
     rail/strip copies in App.svelte (Svelte scopes styles per component, so
     the rule and its keyframe are repeated, not shared — keep them in step). */
  .unread-dot {
    flex: none;
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: color-mix(in srgb, var(--fg) 75%, transparent);
    box-shadow: 0 0 0 2.5px color-mix(in srgb, var(--fg) 10%, transparent);
    animation: unreadfade 0.3s ease-out;
  }
  @media (prefers-reduced-motion: reduce) {
    .unread-dot {
      animation: none;
    }
  }
  @keyframes unreadfade {
    from {
      opacity: 0;
    }
  }

  /* Session state dot — the same modifier vocabulary as the rail. */
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
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 16%, transparent);
    animation: pulse 2.4s ease-in-out infinite;
  }
  .dot.attn {
    background: var(--warn);
    opacity: 1;
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--warn) 16%, transparent);
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
    width: 6px;
    height: 6px;
  }
  .dot.unk {
    opacity: 0.8;
  }
  .dot.starting {
    background: transparent;
    border: 1.5px solid var(--muted);
    opacity: 0.9;
    width: 6px;
    height: 6px;
  }
  @media (prefers-reduced-motion: reduce) {
    .dot.alive {
      animation: none;
    }
  }

  .now {
    font-family: var(--mono);
    font-size: var(--text-sm);
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* The stall warning wears the warn token (the attn/output-only family) —
     a caution about a stale claim, not an error. */
  .now.stalled {
    color: var(--warn);
    cursor: help;
  }
  /* The compact-density stall marker — same warn voice as the full line. */
  .stall-mark {
    flex: none;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--warn);
    border: 1px solid color-mix(in srgb, var(--warn) 45%, transparent);
    border-radius: 999px;
    padding: 0 6px;
    cursor: help;
  }

  .plan {
    display: flex;
    flex-direction: column;
    gap: 3px;
    padding: 2px 0;
  }
  .plan-row {
    display: flex;
    align-items: baseline;
    gap: 8px;
    min-width: 0;
    font-size: var(--text-sm);
    color: var(--muted);
  }
  .plan-mark {
    flex: none;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    border: 1.5px solid var(--edge);
    transform: translateY(-1px);
  }
  .plan-row.in_progress .plan-mark {
    border-color: var(--accent);
    background: var(--accent);
    animation: pulse 2.4s ease-in-out infinite;
  }
  .plan-row.in_progress {
    color: var(--fg);
  }
  .plan-row.done .plan-mark {
    border-color: var(--accent);
  }
  .plan-row.done .plan-text {
    text-decoration: line-through;
    opacity: 0.7;
  }
  .plan-text {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  @media (prefers-reduced-motion: reduce) {
    .plan-row.in_progress .plan-mark {
      animation: none;
    }
  }

  /* The work drop-down rides the shared WorkTray shell full-bleed, so its
     border-top + tint read as the card's own strip — the same chrome the
     chat trays wear, never a forked copy. Row content mirrors AgentsTray
     (subagent rows) and BackgroundTray (background rows). */
  .work {
    margin: 2px -12px;
  }
  .subname {
    flex: none;
    max-width: 60%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg);
    font-family: var(--mono, monospace);
  }
  .subprog {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--muted);
    font-size: var(--text-xs);
  }
  .subage {
    flex: none;
    color: var(--muted);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }
  .bgname {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg);
    font-family: var(--mono, monospace);
  }
  .bgagents {
    flex: none;
    color: var(--muted);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }
  .bgstatus {
    flex: none;
    color: var(--muted);
    font-size: var(--text-xs);
  }
  .bgelapsed {
    flex: none;
    color: var(--muted);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }

  .meta {
    display: flex;
    align-items: center;
    gap: 10px;
    min-width: 0;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .compact .meta {
    flex: none;
  }
  .ctxwrap {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }
  .ctx {
    position: relative;
    width: 72px;
    height: 3px;
    border-radius: 2px;
    background: var(--edge);
    overflow: hidden;
  }
  .ctx i {
    position: absolute;
    inset: 0 auto 0 0;
    border-radius: 2px;
    background: var(--accent);
  }
  .ctx.hot i {
    background: var(--warn);
  }
  .evidence {
    border: none;
    background: none;
    font: inherit;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: pointer;
    padding: 0;
    border-bottom: 1px dotted var(--muted);
  }
  .evidence:hover {
    color: var(--fg);
    border-bottom-color: var(--fg);
  }
  .age {
    margin-left: auto;
    flex: none;
    opacity: 0.8;
  }
  /* Quiet running-cost figure (statusline heartbeat), riding the footer's
     mono/muted right edge next to the age. */
  .cost {
    flex: none;
    opacity: 0.8;
    font-variant-numeric: tabular-nums;
  }
</style>
