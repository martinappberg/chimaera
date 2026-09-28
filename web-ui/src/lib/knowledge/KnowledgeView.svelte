<script lang="ts">
  /**
   * Knowledge — what the agents recorded (design §5): a read-only view with
   * a fixed, plain-words shape over the structured provider (mycelium's
   * .living/) plus the guidance & memory files. Sections are named for the
   * question they answer; an empty section doesn't render; without a
   * provider the view is Guidance & memory plus one card offering the single
   * action that fills it (attach mycelium). One search box, client-side.
   *
   * Everything here is agent- or file-written: one-liners through
   * inlineMarkdown, bodies through the sanitized Markdown. Chimaera never
   * writes knowledge — "open file" is the user's correction path.
   */
  import { tick } from "svelte";
  import Markdown from "../chat/Markdown.svelte";
  import type { LayoutCtrl } from "../layout/dnd";
  import { inlineMarkdown } from "../shared/inlineMarkdown";
  import { pageVisible } from "../shared/visibility";
  import { formatMessageTimestamp } from "../shared/time";
  import { knowledge, knowledgeAvailable, knowledgeError, refreshKnowledge } from "../workspace/knowledge";
  import { timelineStore } from "../workspace/timeline.svelte";
  import { knowledgeProviderActive, openAttachSheet } from "../plugins/store";
  import FindingRow from "./FindingRow.svelte";
  import Ladder from "./Ladder.svelte";
  import {
    LADDER_LEGEND,
    byDateDesc,
    categoryTone,
    recentStatusMove,
    searchKnowledge,
    sectionNav,
    todoTone,
    type SectionKey,
  } from "./model";

  interface Props {
    /** Accepted for symmetry with the other workspace singletons; the
     *  knowledge store already follows the active workspace. */
    wsId?: string | null;
    wsRoot: string | null;
    paneId: string;
    ctrl: LayoutCtrl;
    /** False while this retained tab is behind another pane tab. */
    visible?: boolean;
  }

  let { wsRoot, paneId, ctrl, visible = true }: Props = $props();

  let query = $state("");
  let section = $state<"all" | SectionKey>("all");
  /** The one expanded finding (id), decision (fp) and learning (fp). */
  let openFinding = $state<string | null>(null);
  let openDecision = $state<string | null>(null);
  let openLearning = $state<string | null>(null);

  const raw = $derived($knowledge);
  const shown = $derived(raw !== null ? searchKnowledge(raw, query) : null);
  const nav = $derived(shown !== null ? sectionNav(shown) : []);
  const providerActive = $derived($knowledgeProviderActive && raw?.provider !== null);
  const show = (k: SectionKey): boolean => section === "all" || section === k;
  // A section whose rows vanished (search, a new workspace) falls back.
  $effect(() => {
    if (section !== "all" && !nav.some((n) => n.key === section)) section = "all";
  });

  // Refetch on return: knowledge changes land at episode ends (the timeline
  // nudge covers those), but a hand edit in another window doesn't nudge —
  // a focus return is the cheap moment to catch it.
  let wasVisible = false;
  let primed = false;
  $effect(() => {
    const on = visible && $pageVisible;
    if (primed && on && !wasVisible) refreshKnowledge();
    primed = true;
    wasVisible = on;
  });

  let now = $state(Date.now());
  $effect(() => {
    if (!visible || !$pageVisible) return;
    now = Date.now();
    const t = setInterval(() => (now = Date.now()), 60_000);
    return () => clearInterval(t);
  });

  function abs(p: string): string {
    return p.startsWith("/") || p.startsWith("~") || wsRoot === null ? p : `${wsRoot}/${p}`;
  }
  function openFile(p: string): void {
    ctrl.openFileFrom(paneId, abs(p), false);
  }

  /** An open question links to its finding: expand it and scroll there. */
  async function revealFinding(id: string): Promise<void> {
    if (section !== "all" && section !== "found") section = "all";
    openFinding = id;
    await tick();
    document.getElementById(`finding-${id}`)?.scrollIntoView({ block: "center", behavior: "smooth" });
  }

  const handoffAge = $derived(
    raw?.left_off?.written_ms ? formatMessageTimestamp(raw.left_off.written_ms, now) : null,
  );

  const counts = $derived(shown?.counts ?? null);
</script>

<div class="knowledge">
  <div class="inner">
    <header class="head">
      <div class="titles">
        <h1>Knowledge</h1>
        {#if raw !== null && providerActive && counts !== null}
          <p class="sub">
            What your agents have recorded in this project —
            <span class="mono">{counts.findings}</span> finding{counts.findings === 1 ? "" : "s"} ·
            <span class="mono">{counts.decisions}</span> decision{counts.decisions === 1 ? "" : "s"} ·
            <span class="mono">{counts.learnings}</span> learning{counts.learnings === 1 ? "" : "s"}{#if raw.todos.length > 0}{" · "}<span
                class="mono">{raw.todos.length}</span> to do{/if}{#if raw.questions.length > 0}{" · "}<span
                class="mono">{raw.questions.length}</span> open question{raw.questions.length === 1 ? "" : "s"}{/if}
          </p>
        {:else}
          <p class="sub">What your agents are told — and what they could record here.</p>
        {/if}
      </div>
      <div class="tools">
        {#if raw !== null && providerActive && raw.provider}
          <span class="chip mono">{raw.provider} · .living/ · read-only</span>
        {/if}
        {#if raw !== null && (providerActive || raw.guidance.length > 0)}
          <label class="search">
            <span class="sr">Search knowledge</span>
            <input
              type="search"
              placeholder={providerActive ? "Search findings, decisions, learnings…" : "Search…"}
              bind:value={query}
              spellcheck="false"
            />
          </label>
        {/if}
      </div>
    </header>

    {#if $knowledgeAvailable === false}
      <p class="empty">This daemon has no Knowledge yet — update chimaera to read what your agents record.</p>
    {:else if $knowledgeError !== null && raw === null}
      <p class="empty err">{$knowledgeError}</p>
    {:else if raw === null || shown === null}
      <p class="empty">loading…</p>
    {:else}
      {#if raw.warnings.length > 0}
        <div class="warnings">
          {#each raw.warnings as w, i (i)}<div>{w}</div>{/each}
        </div>
      {/if}

      <div class="grid" class:solo={!providerActive}>
        {#if providerActive}
          <nav class="nav" aria-label="Sections">
            <button class="nrow" class:on={section === "all"} aria-current={section === "all"} onclick={() => (section = "all")}>
              Everything
            </button>
            {#each nav as n (n.key)}
              <button class="nrow" class:on={section === n.key} aria-current={section === n.key} onclick={() => (section = n.key)}>
                {n.label}
                {#if n.count !== null}<span class="count mono">{n.count}</span>{/if}
              </button>
            {/each}
            <div class="legend">
              <div class="lbl">How sure</div>
              {#each LADDER_LEGEND as l (l.status)}
                <div class="lrow"><Ladder status={l.status} />{l.text}</div>
              {/each}
              <div class="lnote">Set by {raw.provider} from each finding's evidence, never by hand.</div>
            </div>
          </nav>
        {/if}

        <div class="sections">
          {#if !providerActive}
            <!-- No structured provider: one card, one action. -->
            <div class="attach-card">
              <div class="atitle">Want findings, decisions and learnings here?</div>
              <p>
                Your agents can record them as they work with mycelium — structured entries in your repo's
                <span class="mono">.living/</span>, with evidence-derived confidence. Chimaera reads them; nothing is curated.
              </p>
              <button class="opt primary" onclick={() => openAttachSheet("mycelium")}>Use mycelium for Knowledge →</button>
            </div>
          {/if}

          {#if providerActive && shown.left_off !== null && show("left")}
            {@const lo = shown.left_off}
            <section aria-labelledby="k-left">
              <h2 id="k-left" class="lbl">Where we left off</h2>
              <div class="card left">
                <div class="col">
                  <div class="ctitle">Current state</div>
                  <div class="md lead"><Markdown text={lo.current || lo.worked_on || "—"} {visible} /></div>
                </div>
                <div class="col">
                  {#if lo.next.length > 0}
                    <div class="ctitle">Next steps</div>
                    <ol class="next">
                      {#each lo.next as n, i (i)}
                        <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
                        <li>{@html inlineMarkdown(n)}</li>
                      {/each}
                    </ol>
                  {/if}
                  {#each lo.blockers as b, i (i)}
                    <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
                    <div class="blocked">Blocked: {@html inlineMarkdown(b)}</div>
                  {/each}
                </div>
                <div class="cfoot">
                  <span>
                    Handoff{#if lo.by} written by <span class="mono">{lo.by.name}</span>{/if}{#if handoffAge} · {handoffAge}{/if}
                  </span>
                  <button class="link" onclick={() => openFile(lo.path)}>open handoff</button>
                </div>
              </div>
            </section>
          {/if}

          {#if providerActive && shown.topics.length > 0 && show("found")}
            <section aria-labelledby="k-found" class="found">
              <h2 id="k-found" class="lbl">What we found</h2>
              {#each shown.topics as t (t.slug)}
                <div class="topic">
                  <div class="thead">
                    <span class="mono tslug">{t.slug}</span>
                    <span class="tdesc">{t.description}</span>
                  </div>
                  <div class="card list">
                    {#each t.findings as f (f.id)}
                      <FindingRow
                        finding={f}
                        topic={t.slug}
                        open={openFinding === f.id}
                        badge={recentStatusMove(f.id, timelineStore.entries, now)}
                        onToggle={() => (openFinding = openFinding === f.id ? null : f.id)}
                        onOpenFile={() => openFile(t.path)}
                        filePath={t.path}
                        {visible}
                      />
                    {/each}
                  </div>
                </div>
              {/each}
            </section>
          {/if}

          {#if providerActive && shown.decisions.length > 0 && show("decided")}
            <section aria-labelledby="k-decided">
              <h2 id="k-decided" class="lbl">What we decided</h2>
              <div class="rows">
                {#each byDateDesc(shown.decisions) as d (d.fp)}
                  <div class="drow-wrap">
                    <button class="drow" aria-expanded={openDecision === d.fp} onclick={() => (openDecision = openDecision === d.fp ? null : d.fp)}>
                      <span class="mono muted date">{d.date}</span>
                      <span class="dbody">
                        <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
                        <span class="dtitle">{@html inlineMarkdown(d.title)}</span>
                        {#if d.decision}
                          <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
                          <span class="dtext">{@html inlineMarkdown(d.decision)}</span>
                        {/if}
                        {#if d.alternatives.length > 0}
                          <span class="dalt">instead of {d.alternatives.join(" · ")}</span>
                        {/if}
                      </span>
                      <span class="mono muted by">{d.recorded_by?.name ?? ""}</span>
                    </button>
                    {#if openDecision === d.fp}
                      <div class="detail">
                        {#if d.context}<div class="block"><div class="lbl small">Context</div><div class="md"><Markdown text={d.context} {visible} /></div></div>{/if}
                        {#if d.rationale}<div class="block"><div class="lbl small">Why</div><div class="md"><Markdown text={d.rationale} {visible} /></div></div>{/if}
                        {#if d.consequences}<div class="block"><div class="lbl small">Consequences</div><div class="md"><Markdown text={d.consequences} {visible} /></div></div>{/if}
                        <div class="foot">
                          <button class="link mono" onclick={() => openFile(".living/decisions.md")}>.living/decisions.md</button>
                          {#if d.line > 0}<span class="muted">line {d.line}</span>{/if}
                        </div>
                      </div>
                    {/if}
                  </div>
                {/each}
              </div>
            </section>
          {/if}

          {#if providerActive && shown.learnings.length > 0 && show("watch")}
            <section aria-labelledby="k-watch">
              <h2 id="k-watch" class="lbl">Watch out for</h2>
              <div class="watch">
                {#each byDateDesc(shown.learnings) as l (l.fp)}
                  <button class="lcard" class:open={openLearning === l.fp} aria-expanded={openLearning === l.fp} onclick={() => (openLearning = openLearning === l.fp ? null : l.fp)}>
                    <span class="lhead">
                      <span class="cat mono {categoryTone(l.category)}">{l.category}</span>
                      {#if l.tags.length > 0}<span class="ltags mono">{l.tags.join(" · ")}</span>{/if}
                    </span>
                    <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
                    <span class="ltitle">{@html inlineMarkdown(l.title)}</span>
                    {#if l.why}
                      <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
                      <span class="lwhy">{@html inlineMarkdown(l.why)}</span>
                    {/if}
                    {#if openLearning === l.fp}
                      <span class="lmore">
                        {#if l.what}<span class="block"><span class="lbl small">What happened</span><span class="md"><Markdown text={l.what} {visible} /></span></span>{/if}
                        {#if l.resolution}<span class="block"><span class="lbl small">Resolution</span><span class="md"><Markdown text={l.resolution} {visible} /></span></span>{/if}
                        <span class="foot">
                          <span class="mono muted">{l.date}</span>
                          {#if l.recorded_by}<span class="muted">recorded by <span class="mono">{l.recorded_by.name}</span></span>{/if}
                          <span
                            class="link mono"
                            role="link"
                            tabindex="0"
                            onclick={(e) => {
                              e.stopPropagation();
                              openFile(".living/learnings.md");
                            }}
                            onkeydown={(e) => {
                              if (e.key === "Enter") {
                                e.stopPropagation();
                                openFile(".living/learnings.md");
                              }
                            }}>.living/learnings.md</span
                          >
                        </span>
                      </span>
                    {/if}
                  </button>
                {/each}
              </div>
            </section>
          {/if}

          {#if providerActive && shown.todos.length + shown.questions.length > 0 && show("open")}
            <section aria-labelledby="k-open">
              <h2 id="k-open" class="lbl">To do &amp; questions</h2>
              <div class="open" class:two={shown.todos.length > 0 && shown.questions.length > 0}>
                {#if shown.todos.length > 0}
                  <div class="ocol">
                    <div class="ctitle">To do</div>
                    {#each shown.todos as t, i (i)}
                      <div class="trow">
                        <span class="mono muted prio">{t.priority}</span>
                        <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
                        <span class="titem">{@html inlineMarkdown(t.item)}</span>
                        <span class="tstatus {todoTone(t.status)}">{t.status}</span>
                      </div>
                    {/each}
                    <div class="foot pad">
                      <button class="link mono" onclick={() => openFile("todo/TODO_REGISTRY.md")}>todo/TODO_REGISTRY.md</button>
                    </div>
                  </div>
                {/if}
                {#if shown.questions.length > 0}
                  <div class="ocol">
                    <div class="ctitle">Open questions</div>
                    {#each shown.questions as q, i (i)}
                      <div class="qrow">
                        <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
                        <span class="qtext">{@html inlineMarkdown(q.text)}</span>
                        {#if q.finding}
                          <button class="link mono" onclick={() => void revealFinding(q.finding)}>{q.finding}</button>
                        {/if}
                      </div>
                    {/each}
                  </div>
                {/if}
              </div>
            </section>
          {/if}

          {#if shown.guidance.length > 0 && show("guide")}
            <section aria-labelledby="k-guide">
              <h2 id="k-guide" class="lbl">Guidance & memory</h2>
              <div class="guide">
                {#each shown.guidance as g (g.path)}
                  <button class="gcard" onclick={() => openFile(g.path)} title={g.path}>
                    <span class="mono glabel">{g.label}</span>
                    {#if g.description}<span class="gdesc">{g.description}</span>{/if}
                  </button>
                {/each}
              </div>
            </section>
          {:else if !providerActive && shown.guidance.length === 0}
            <p class="empty">No AGENTS.md, CLAUDE.md or agent memory here yet.</p>
          {/if}

          {#if providerActive && query.trim() !== "" && nav.length === 0}
            <p class="empty">Nothing matches “{query.trim()}”.</p>
          {/if}
        </div>
      </div>
    {/if}
  </div>
</div>

<style>
  .knowledge {
    position: absolute;
    inset: 0;
    overflow-y: auto;
    background: var(--bg);
    container-type: inline-size;
  }
  .inner {
    max-width: 1180px;
    margin: 0 auto;
    padding: 26px 36px 48px;
    display: flex;
    flex-direction: column;
    gap: 22px;
  }

  .head {
    display: flex;
    align-items: flex-end;
    gap: 24px;
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
  .mono {
    font-family: var(--mono);
    font-size: 0.95em;
  }
  .muted {
    color: var(--muted);
  }
  .tools {
    margin-left: auto;
    display: flex;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
  }
  .chip {
    font-size: 11.5px;
    color: var(--muted);
    padding: 3px 9px;
    border: 1px solid var(--edge);
    border-radius: 999px;
    white-space: nowrap;
  }
  .search {
    display: flex;
  }
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
  }
  .search input {
    width: 280px;
    max-width: 60vw;
    border: 1px solid var(--edge);
    background: var(--overlay-bg);
    color: var(--fg);
    border-radius: 8px;
    padding: 7px 12px;
    font: inherit;
    font-size: var(--text-sm);
  }
  .search input::placeholder {
    color: var(--muted);
    opacity: 0.8;
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
  .warnings {
    font-size: var(--text-xs);
    color: var(--warn);
    line-height: 1.5;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .grid {
    display: grid;
    grid-template-columns: 196px minmax(0, 1fr);
    gap: 40px;
    align-items: start;
  }
  .grid.solo {
    grid-template-columns: minmax(0, 1fr);
  }
  @container (max-width: 820px) {
    .grid {
      grid-template-columns: minmax(0, 1fr);
      gap: 22px;
    }
    .nav {
      position: static;
      flex-direction: row;
      flex-wrap: wrap;
    }
    .legend {
      display: none;
    }
  }

  .nav {
    display: flex;
    flex-direction: column;
    gap: 2px;
    position: sticky;
    top: 0;
  }
  .nrow {
    display: flex;
    align-items: center;
    gap: 8px;
    text-align: left;
    appearance: none;
    border: none;
    background: none;
    padding: 7px 10px;
    border-radius: 6px;
    font: inherit;
    font-size: var(--text-sm);
    color: var(--muted);
    cursor: pointer;
  }
  .nrow:hover {
    color: var(--fg);
    background: var(--row-hover);
  }
  .nrow.on {
    background: var(--row-active);
    color: var(--fg);
    font-weight: 500;
  }
  .count {
    margin-left: auto;
    font-size: 11.5px;
    color: var(--muted);
  }
  .legend {
    margin-top: 22px;
    padding: 12px 10px 0;
    border-top: 1px solid var(--edge);
    display: flex;
    flex-direction: column;
    gap: 7px;
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .lrow {
    display: flex;
    gap: 8px;
    align-items: center;
  }
  .lnote {
    line-height: 1.45;
    padding-top: 4px;
    opacity: 0.85;
  }

  .lbl {
    margin: 0;
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
    font-weight: 600;
  }
  .lbl.small {
    font-size: 10.5px;
  }

  .sections {
    display: flex;
    flex-direction: column;
    gap: 34px;
    min-width: 0;
    max-width: 920px;
  }
  section {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  section.found {
    gap: 18px;
  }

  .attach-card {
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 12px;
    padding: 18px 20px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    align-items: flex-start;
    max-width: 640px;
  }
  .atitle {
    font-size: var(--text-md);
    font-weight: 600;
  }
  .attach-card p {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--muted);
    line-height: 1.5;
  }
  .attach-card .opt {
    margin-top: 4px;
  }

  .card {
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 12px;
  }
  .card.left {
    padding: 18px 22px;
    display: grid;
    grid-template-columns: minmax(0, 1.1fr) minmax(0, 1fr);
    gap: 28px;
  }
  @container (max-width: 760px) {
    .card.left {
      grid-template-columns: minmax(0, 1fr);
    }
  }
  .card.list {
    overflow: hidden;
  }
  .col {
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-width: 0;
  }
  .ctitle {
    font-size: var(--text-xs);
    color: var(--muted);
    font-weight: 600;
  }
  .md {
    font-size: var(--text-md);
    line-height: 1.5;
  }
  .md.lead {
    font-size: 15px;
    line-height: 1.55;
  }
  .md :global(p) {
    margin: 0 0 0.5em;
  }
  .md :global(p:last-child) {
    margin-bottom: 0;
  }
  .next {
    margin: 0;
    padding-left: 18px;
    font-size: var(--text-md);
    line-height: 1.7;
  }
  .next :global(code),
  .blocked :global(code),
  .dtitle :global(code),
  .dtext :global(code),
  .ltitle :global(code),
  .lwhy :global(code),
  .titem :global(code),
  .qtext :global(code) {
    font-family: var(--mono);
    font-size: 0.92em;
  }
  .blocked {
    font-size: var(--text-sm);
    color: var(--warn);
    background: color-mix(in srgb, var(--warn) 9%, transparent);
    padding: 7px 10px;
    border-radius: 6px;
    margin-top: 2px;
    line-height: 1.45;
  }
  .cfoot {
    grid-column: 1 / -1;
    display: flex;
    gap: 10px;
    font-size: var(--text-xs);
    color: var(--muted);
    border-top: 1px solid var(--edge);
    padding-top: 12px;
  }
  .cfoot .link {
    margin-left: auto;
  }

  .topic {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .thead {
    display: flex;
    align-items: baseline;
    gap: 12px;
    padding-bottom: 6px;
  }
  .tslug {
    font-size: 12.5px;
    font-weight: 600;
  }
  .tdesc {
    font-size: var(--text-sm);
    color: var(--muted);
  }

  .rows {
    display: flex;
    flex-direction: column;
  }
  .drow-wrap {
    border-top: 1px solid var(--edge);
  }
  .drow {
    width: 100%;
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    color: var(--fg);
    text-align: left;
    cursor: pointer;
    display: grid;
    grid-template-columns: 96px minmax(0, 1fr) auto;
    column-gap: 18px;
    padding: 13px 6px;
    margin: 0 -6px;
    width: calc(100% + 12px);
    border-radius: 8px;
    transition: background-color 0.12s ease;
  }
  .drow:hover {
    background: var(--row-hover);
  }
  @container (max-width: 760px) {
    .drow {
      grid-template-columns: minmax(0, 1fr);
      row-gap: 4px;
    }
  }
  .date {
    font-size: var(--text-xs);
    padding-top: 2px;
  }
  .dbody {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }
  .dtitle {
    font-size: var(--text-md);
    font-weight: 600;
  }
  .dtext {
    font-size: var(--text-md);
    line-height: 1.5;
  }
  .dalt {
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .by {
    font-size: var(--text-xs);
  }
  .detail {
    margin: 0 0 14px 96px;
    background: color-mix(in srgb, var(--fg) 3%, transparent);
    border-radius: 10px;
    padding: 14px 18px;
    display: flex;
    flex-direction: column;
    gap: 12px;
    animation: rise 0.18s ease;
  }
  @container (max-width: 760px) {
    .detail {
      margin-left: 0;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .detail {
      animation: none;
    }
  }
  .block {
    display: flex;
    flex-direction: column;
    gap: 5px;
  }
  .foot {
    display: flex;
    gap: 14px;
    font-size: var(--text-xs);
    align-items: baseline;
  }
  .foot.pad {
    padding-top: 8px;
  }
  .link {
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--accent);
    cursor: pointer;
  }
  .link:hover {
    text-decoration: underline;
  }

  .watch {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 12px;
  }
  @container (max-width: 760px) {
    .watch {
      grid-template-columns: minmax(0, 1fr);
    }
  }
  .lcard {
    appearance: none;
    font: inherit;
    color: var(--fg);
    text-align: left;
    cursor: pointer;
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 10px;
    padding: 14px 16px;
    display: flex;
    flex-direction: column;
    gap: 7px;
    min-width: 0;
    transition: border-color 0.12s ease;
  }
  .lcard:hover,
  .lcard.open {
    border-color: color-mix(in srgb, var(--accent) 45%, var(--edge));
  }
  .lhead {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .cat {
    font-size: 11px;
    padding: 1px 8px;
    border-radius: 999px;
    color: var(--muted);
    border: 1px solid var(--edge);
  }
  .cat.warn {
    color: var(--warn);
    background: color-mix(in srgb, var(--warn) 10%, transparent);
    border-color: transparent;
  }
  .cat.err {
    color: var(--err);
    background: color-mix(in srgb, var(--err) 10%, transparent);
    border-color: transparent;
  }
  .cat.accent {
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    border-color: transparent;
  }
  .ltags {
    margin-left: auto;
    font-size: 11px;
    color: var(--muted);
    opacity: 0.8;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .ltitle {
    font-size: var(--text-md);
    font-weight: 600;
    line-height: 1.4;
  }
  .lwhy {
    font-size: var(--text-sm);
    color: var(--muted);
    line-height: 1.5;
  }
  .lmore {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding-top: 6px;
    border-top: 1px solid var(--edge);
    margin-top: 2px;
    font-size: var(--text-sm);
  }
  .lmore .block {
    display: flex;
  }

  .open {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 28px;
  }
  .open.two {
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
  }
  @container (max-width: 760px) {
    .open.two {
      grid-template-columns: minmax(0, 1fr);
    }
  }
  .ocol {
    display: flex;
    flex-direction: column;
  }
  .ocol .ctitle {
    padding-bottom: 8px;
  }
  .trow {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 0;
    border-top: 1px solid var(--edge);
    font-size: var(--text-md);
  }
  .prio {
    width: 52px;
    flex: none;
    font-size: 11px;
  }
  .titem {
    flex: 1;
    min-width: 0;
  }
  .tstatus {
    flex: none;
    font-size: 11.5px;
    color: var(--muted);
  }
  .tstatus.warn {
    color: var(--warn);
    background: color-mix(in srgb, var(--warn) 10%, transparent);
    padding: 1px 8px;
    border-radius: 999px;
  }
  .tstatus.accent {
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    padding: 1px 8px;
    border-radius: 999px;
  }
  .qrow {
    display: flex;
    align-items: baseline;
    gap: 10px;
    padding: 10px 0;
    border-top: 1px solid var(--edge);
    font-size: var(--text-md);
  }
  .qtext {
    flex: 1;
    min-width: 0;
  }

  .guide {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(190px, 1fr));
    gap: 10px;
  }
  .gcard {
    appearance: none;
    font: inherit;
    color: var(--fg);
    text-align: left;
    cursor: pointer;
    background: none;
    border: 1px solid var(--edge);
    border-radius: 10px;
    padding: 12px 14px;
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
    transition: border-color 0.12s ease;
  }
  .gcard:hover {
    border-color: color-mix(in srgb, var(--accent) 45%, var(--edge));
  }
  .glabel {
    font-size: 12.5px;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .gdesc {
    font-size: 12.5px;
    color: var(--muted);
    line-height: 1.4;
  }
</style>
