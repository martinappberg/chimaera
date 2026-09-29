<script lang="ts">
  /**
   * Knowledge's first screen, four answers top to bottom: where we left off
   * (the newest handoff, as written), what is waiting on the user, what
   * changed this week (bad news first), and open work. Each collapses to one
   * honest line when there's nothing to say.
   */
  import { formatMessageTimestamp } from "../shared/time";
  import { inlineMarkdown } from "../shared/inlineMarkdown";
  import type { Knowledge, KnowledgeLabels } from "../workspace/knowledge";
  import type { RefMatcher, ReferenceChips } from "../shared/references";
  import Badge from "../shared/ui/Badge.svelte";
  import { qualifiedId, type KnowledgeIndex } from "./entries";
  import EntryBody from "./EntryBody.svelte";
  import type { Section } from "./list";
  import {
    kindWord,
    newestDate,
    openWork,
    priorityTone,
    stateLabel,
    TODO_GROUP_LABEL,
    todoGroup,
    waitingOnYou,
    whatChanged,
  } from "./overview";

  interface Props {
    k: Knowledge;
    idx: KnowledgeIndex;
    matcher: RefMatcher | null;
    labels: KnowledgeLabels;
    chips: ReferenceChips;
    wsRoot: string | null;
    wsId: string | null;
    today: string;
    now: number;
    onOpen: (ekey: string) => void;
    onOpenFile: (path: string, line: number, end: number) => void;
    onSection: (s: Section) => void;
  }

  let { k, idx, matcher, labels, chips, wsRoot, wsId, today, now, onOpen, onOpenFile, onSection }: Props = $props();

  const lo = $derived(k.left_off);
  const waiting = $derived(waitingOnYou(k, idx, 5));
  const changed = $derived(whatChanged(idx, today, 7));
  const newest = $derived(newestDate(idx));
  const work = $derived(openWork(idx, 6));
  let olderOpen = $state(false);

  function dayLabel(day: string): string {
    if (day === today) return "Today";
    const [y, m, d] = today.split("-").map(Number);
    const yesterday = new Date(y, m - 1, d - 1);
    const p = (n: number): string => String(n).padStart(2, "0");
    if (day === `${yesterday.getFullYear()}-${p(yesterday.getMonth() + 1)}-${p(yesterday.getDate())}`) return "Yesterday";
    const [yy, mm, dd] = day.split("-").map(Number);
    return new Date(yy, mm - 1, dd).toLocaleDateString(undefined, { weekday: "long", month: "short", day: "numeric" });
  }

  function age(ms: number): string {
    return ms > 0 ? formatMessageTimestamp(ms, now) : "";
  }

  const slots = $derived.by(() => {
    if (lo === null) return [];
    const out: { label: string; text: string }[] = [];
    if (lo.current) out.push({ label: "Now", text: lo.current });
    if (lo.worked_on && lo.worked_on !== lo.current) out.push({ label: "Done", text: lo.worked_on });
    if (lo.next.length > 0) out.push({ label: "Next", text: lo.next.map((n, i) => `${i + 1}. ${n}`).join("\n") });
    if (lo.blockers.length > 0) out.push({ label: "Blocked", text: lo.blockers.map((b) => `- ${b}`).join("\n") });
    if (lo.decisions) out.push({ label: "Decided", text: lo.decisions });
    return out;
  });
</script>

<div class="overview">
  <section aria-labelledby="ov-left">
    <h2 id="ov-left" class="lbl">{labels.sections.left_off}</h2>
    {#if lo === null}
      <p class="quiet">No session handoff yet — agents write one when a session ends.</p>
    {:else}
      <div class="card handoff">
        <div class="hmeta">
          <span>Handoff{#if lo.written_ms > 0} · {age(lo.written_ms)}{/if}</span>
          {#if lo.session_id}<span class="mono" title="The agent session that wrote it">session {lo.session_id.slice(0, 8)}</span>{/if}
          <button class="link mono path" onclick={() => onOpenFile(lo.path, 0, 0)} title={lo.path}>{lo.path} ↗</button>
        </div>
        {#if slots.length === 0}
          <p class="quiet">The handoff says nothing beyond its heading — <button class="link" onclick={() => onOpenFile(lo.path, 0, 0)}>open it</button>.</p>
        {:else}
          <dl class="slots">
            {#each slots as s (s.label)}
              <dt>{s.label}</dt>
              <dd>
                <EntryBody span={null} fallback={s.text} {matcher} {chips} {wsRoot} {wsId} owner={k} />
              </dd>
            {/each}
          </dl>
        {/if}
        {#if lo.sources.length > 1}
          <div class="older">
            <button class="link muted" onclick={() => (olderOpen = !olderOpen)} aria-expanded={olderOpen}>
              {lo.sources.length - 1} older handoff{lo.sources.length === 2 ? "" : "s"}
            </button>
            {#if olderOpen}
              {#each lo.sources.slice(1) as src (src.path)}
                <button class="orow" onclick={() => onOpenFile(src.path, 0, 0)}>
                  <span>{age(src.written_ms)}</span>
                  <span class="mono">{src.path}</span>
                </button>
              {/each}
            {/if}
          </div>
        {/if}
      </div>
    {/if}
  </section>

  {#if waiting.total > 0}
    <section aria-labelledby="ov-asks">
      <h2 id="ov-asks" class="lbl">{labels.sections.asks} <span class="n">{waiting.total}</span></h2>
      <div class="rows">
        {#each waiting.items as w, i (i)}
          <button
            class="row ask"
            onclick={() => (w.entry !== null ? onOpen(w.entry.ekey) : w.span !== null ? onOpenFile(w.span.path, w.span.line, w.span.end_line) : undefined)}
          >
            <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
            <span class="q">{@html inlineMarkdown(w.text)}</span>
            <span class="src">
              {#if w.entry !== null}<span class="mono">{qualifiedId(idx, w.entry)}</span>{:else}<span>{w.sourceLabel}</span>{/if}
              {#if w.date}<span class="mono d">{w.date.slice(5, 10)}</span>{/if}
            </span>
          </button>
        {/each}
      </div>
    </section>
  {/if}

  <section aria-labelledby="ov-changed">
    <h2 id="ov-changed" class="lbl">{labels.sections.changed} <span class="n">7 days</span></h2>
    {#if changed.length === 0}
      <p class="quiet">
        Nothing recorded in the last 7 days{newest !== "" ? ` — the newest entry is from ${newest}` : ""}.
      </p>
    {:else}
      <div class="rows">
        {#each changed as d (d.day)}
          <div class="day">{dayLabel(d.day)}</div>
          {#each d.entries as e (e.ekey)}
            {@const st = stateLabel(e)}
            <button class="row" onclick={() => onOpen(e.ekey)}>
              <span class="line">
                <span class="mono id">{qualifiedId(idx, e)}</span>
                <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
                <span class="t">{@html inlineMarkdown(e.title)}</span>
              </span>
              <span class="src">
                {#each e.amends as a (a.kind + a.id)}<Badge text="{a.kind} {a.id}" tone="warn" />{/each}
                {#if st !== null}<Badge text={st.text} tone={st.tone} />{/if}
                {#if e.amends.length === 0 && st === null}<Badge text={kindWord(labels, e.kind)} />{/if}
                {#if e.recordedBy !== null}<span>{e.recordedBy.name}</span>{/if}
              </span>
            </button>
          {/each}
        {/each}
      </div>
    {/if}
  </section>

  {#if work.open > 0}
    <section aria-labelledby="ov-work">
      <h2 id="ov-work" class="lbl">
        {labels.sections.open_work}
        <span class="n">{work.open} open{#if work.progress > 0} · {work.progress} in progress{/if}{#if work.blocked > 0} · {work.blocked} blocked{/if}</span>
        <button class="more" onclick={() => onSection("todos")}>{labels.sections.todos} →</button>
      </h2>
      <div class="rows">
        {#each work.items as e (e.ekey)}
          {#if e.todo !== undefined}
            {@const g = todoGroup(e.todo)}
            <button class="row" onclick={() => onOpen(e.ekey)}>
              <span class="line">
                <span class="prio {priorityTone(e.todo.priority)}" title={e.todo.priority}></span>
                <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
                <span class="t">{@html inlineMarkdown(e.title)}</span>
              </span>
              <span class="src">
                <Badge text={TODO_GROUP_LABEL[g].toLowerCase()} tone={g === "blocked" ? "warn" : g === "progress" ? "good" : "neutral"} />
                {#if e.todo.id !== ""}<span class="mono">{e.todo.id}</span>{/if}
              </span>
            </button>
          {/if}
        {/each}
      </div>
    </section>
  {/if}
</div>

<style>
  .overview {
    display: flex;
    flex-direction: column;
    gap: 26px;
    max-width: 980px;
    min-width: 0;
  }
  .lbl {
    margin: 0 0 8px;
    display: flex;
    align-items: baseline;
    gap: 8px;
    font-size: 10.5px;
    font-weight: 600;
    letter-spacing: 0.09em;
    text-transform: uppercase;
    color: var(--muted);
  }
  .n {
    font-family: var(--mono);
    letter-spacing: 0;
    text-transform: none;
    font-weight: 500;
  }
  .more {
    margin-left: auto;
    border: 0;
    background: none;
    padding: 0;
    font: inherit;
    font-size: var(--text-xs);
    letter-spacing: 0;
    text-transform: none;
    font-weight: 500;
    color: var(--accent);
    cursor: pointer;
  }
  .quiet {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--muted);
  }
  .card {
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 10px;
  }
  .handoff {
    padding: 14px 18px 12px;
  }
  .hmeta {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 12px;
    align-items: baseline;
    font-size: var(--text-xs);
    color: var(--muted);
    margin-bottom: 10px;
  }
  .hmeta .path {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 100%;
  }
  .slots {
    margin: 0;
    display: grid;
    grid-template-columns: 76px minmax(0, 1fr);
    gap: 8px 16px;
  }
  .slots dt {
    font-size: var(--text-xs);
    color: var(--muted);
    padding-top: 3px;
  }
  .slots dd {
    margin: 0;
    min-width: 0;
  }
  .slots dd :global(.body) {
    font-size: var(--text-md);
    line-height: 1.55;
  }
  .older {
    margin-top: 12px;
    padding-top: 8px;
    border-top: 1px solid var(--edge);
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: var(--text-xs);
  }
  .orow {
    display: flex;
    gap: 10px;
    border: 0;
    background: none;
    padding: 2px 0;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-xs);
    text-align: left;
    cursor: pointer;
  }
  .orow:hover {
    color: var(--fg);
  }
  .link {
    border: 0;
    background: none;
    padding: 0;
    color: var(--accent);
    font: inherit;
    cursor: pointer;
    text-align: left;
  }
  .link.muted {
    color: var(--muted);
  }
  .link:hover {
    text-decoration: underline;
  }
  .mono {
    font-family: var(--mono);
    font-size: 0.95em;
  }
  .rows {
    border-top: 1px solid var(--edge);
    display: flex;
    flex-direction: column;
  }
  .day {
    font-size: var(--text-xs);
    font-weight: 500;
    color: var(--muted);
    padding: 10px 6px 3px;
  }
  .row {
    display: flex;
    align-items: baseline;
    gap: 6px 16px;
    flex-wrap: wrap;
    width: 100%;
    padding: 8px 6px;
    border: 0;
    border-bottom: 1px solid var(--edge);
    background: none;
    color: var(--fg);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .row:hover {
    background: var(--row-hover);
  }
  .line {
    flex: 1 1 320px;
    min-width: 0;
    display: flex;
    gap: 10px;
    align-items: baseline;
  }
  .id {
    color: var(--muted);
    flex: none;
  }
  .t {
    font-size: var(--text-sm);
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .t :global(code),
  .q :global(code) {
    font-family: var(--mono);
    font-size: 0.9em;
  }
  .q {
    flex: 1 1 320px;
    min-width: 0;
    font-size: var(--text-sm);
    line-height: 1.5;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .src {
    margin-left: auto;
    display: flex;
    gap: 8px;
    align-items: baseline;
    font-size: var(--text-xs);
    color: var(--muted);
    white-space: nowrap;
  }
  .d {
    color: var(--muted);
  }
  .prio {
    width: 9px;
    height: 9px;
    border-radius: 2px;
    flex: none;
    align-self: center;
    background: color-mix(in srgb, var(--muted) 55%, transparent);
  }
  .prio.bad {
    background: var(--err);
  }
  .prio.warn {
    background: var(--warn);
  }
  @container (max-width: 560px) {
    .slots {
      grid-template-columns: minmax(0, 1fr);
      gap: 2px 0;
    }
    .slots dd {
      margin-bottom: 8px;
    }
  }
</style>
