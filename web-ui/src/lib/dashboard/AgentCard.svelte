<script lang="ts">
  /**
   * One roster card on the workspace dashboard: who, doing what, produced
   * what — and a door into the live session (the whole card opens it).
   * Renders from server wire truth alone; when a warm chat store is passed
   * (bounded upstream in DashboardView) it adds the live now-line, context
   * meter, plan snapshot, and the subagent drop-down. Zones a card can't
   * truthfully fill stay empty — never fabricated.
   */
  import SessionGlyph from "../shared/SessionGlyph.svelte";
  import Chevron from "../shared/Chevron.svelte";
  import { relativeAge } from "../workspace/launcher";
  import { agentKind, dotState, dotTitle, type Session } from "../workspace/sessions";
  import type { ChatBlock, ChatStore } from "../chat/store.svelte";
  import { provenanceOf, provenanceTitle } from "./dash";

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
    /** Stop a subagent (claude chat only); omitted when unsupported. */
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
    if (store !== null) {
      const act = store.activity;
      if (act !== null && act.detail !== "") return act.detail;
      if (act !== null) return act.kind;
      const step = store.plan.find((p) => p.status === "in_progress");
      if (step !== undefined) return step.content;
      const lastMsg = [...store.blocks].reverse().find((b) => b.kind === "message");
      if (lastMsg !== undefined) return lastMsg.text.slice(0, 160);
    }
    // Hook-level truth: the last file the agent wrote is the freshest signal.
    if (prov === "hooks" && touched.length > 0) {
      return `edited ${basename(touched[touched.length - 1])}`;
    }
    if (!session.alive) return "exited";
    if (prov === "none") return "state unknown — open the terminal";
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

  /** The drop-down: closed by default so ten cards stay scannable (the
   *  single-agent hero opens it); a user toggle overrides either default. */
  let subsToggled = $state<boolean | null>(null);
  const subsOpen = $derived(subsToggled ?? hero);

  /** Driver titles are "Agent: {description}" — the prefix is the tray's. */
  function subName(title: string): string {
    return title.startsWith("Agent: ") ? title.slice(7) : title;
  }
  function subProgress(b: Extract<ChatBlock, { kind: "tool" }>): string {
    return b.content?.kind === "output" ? (b.content.text ?? "").trim() : "";
  }

  function basename(p: string): string {
    const i = p.lastIndexOf("/");
    return i >= 0 ? p.slice(i + 1) : p;
  }
  function relPath(p: string): string {
    return wsRoot !== null && p.startsWith(`${wsRoot}/`) ? p.slice(wsRoot.length + 1) : p;
  }

  const planEntries = $derived(hero && store !== null ? store.plan.slice(0, 6) : []);
  const ctxPct = $derived(store?.contextPct ?? null);

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
    <span class="chip">{agentKind(session)} · {session.ui === "chat" ? "chat" : "term"}</span>
    <span class="prov {prov}" title={provenanceTitle(prov)}>
      {prov === "protocol" ? "protocol" : prov === "hooks" ? "hooks" : "output-only"}
    </span>
  </div>

  {#if !compact && nowLine !== null}
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

  {#if !compact && activeAgents.length > 0}
    <div class="subs">
      <button
        class="subs-head"
        aria-expanded={subsOpen}
        onclick={(e) => {
          e.stopPropagation();
          subsToggled = !subsOpen;
        }}
      >
        <Chevron open={subsOpen} />
        <span class="spark" aria-hidden="true">✳</span>
        <span
          >{activeAgents.length === 1
            ? "subagent working"
            : `${activeAgents.length} subagents working`}</span
        >
      </button>
      {#if subsOpen}
        <div class="subtree">
          {#each activeAgents as a (a.id)}
            <div class="subrow">
              <span class="subdot" aria-hidden="true"></span>
              <span class="subname" title={subName(a.title)}>{subName(a.title)}</span>
              {#if subProgress(a) !== ""}
                <span class="subprog" title={subProgress(a)}>{subProgress(a)}</span>
              {/if}
              {#if onStopTask !== undefined}
                <button
                  class="substop"
                  title="stop this subagent"
                  onclick={(e) => {
                    e.stopPropagation();
                    onStopTask(a.id);
                  }}>stop</button
                >
              {/if}
            </div>
          {/each}
        </div>
      {/if}
    </div>
  {/if}

  <div class="meta">
    {#if ctxPct !== null}
      <span class="ctxwrap" title="context window used">
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

  /* The honesty axis: which tier this card's status truthfully comes from. */
  .prov {
    flex: none;
    font-family: var(--mono);
    font-size: 10px;
    cursor: help;
    opacity: 0.85;
  }
  .prov.protocol {
    color: var(--accent);
  }
  .prov.hooks {
    color: var(--muted);
  }
  .prov.none {
    color: var(--warn);
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

  .subs {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .subs-head {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0;
    border: none;
    background: none;
    font: inherit;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: pointer;
  }
  .subs-head:hover {
    color: var(--fg);
  }
  .spark {
    color: var(--accent);
    animation: pulse 2.4s ease-in-out infinite;
  }
  @media (prefers-reduced-motion: reduce) {
    .spark {
      animation: none;
    }
  }
  .subtree {
    margin-left: 4px;
    padding-left: 10px;
    border-left: 1px solid var(--edge);
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .subrow {
    display: flex;
    align-items: center;
    gap: 7px;
    min-width: 0;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    white-space: nowrap;
  }
  .subdot {
    flex: none;
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: var(--accent);
    animation: pulse 2.4s ease-in-out infinite;
  }
  @media (prefers-reduced-motion: reduce) {
    .subdot {
      animation: none;
    }
  }
  .subname {
    color: var(--fg);
    overflow: hidden;
    text-overflow: ellipsis;
    flex: none;
    max-width: 55%;
  }
  .subprog {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .substop {
    flex: none;
    margin-left: auto;
    border: none;
    background: none;
    font: inherit;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: pointer;
    padding: 0 2px;
  }
  .substop:hover {
    color: var(--err);
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
</style>
