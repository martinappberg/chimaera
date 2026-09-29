<script lang="ts">
  /**
   * "Where things stand" — the top of Knowledge on the dashboard: what is
   * waiting on the user, what changed recently (corrections and
   * supersessions first), and the handoff's next steps and blockers. Every
   * row opens its entry in Knowledge. With no knowledge plugin it is one
   * calm block of plain sentences — what Agent notes and Mycelium would do
   * here — and one link, Extensions. Never a curated summary, never a
   * rating: everything here is what the agents recorded, as written.
   */
  import { inlineMarkdown } from "../shared/inlineMarkdown";
  import { focusKnowledgeEntry, type Knowledge } from "../workspace/knowledge";
  import { qualifiedId } from "../knowledge/entries";
  import { isoDay, stateLabel, waitingOnYou, whatChanged } from "../knowledge/overview";
  import { knowledgeLookup } from "../knowledge/store";

  interface Props {
    knowledge: Knowledge | null;
    /** A knowledge plugin is active in this workspace. */
    providerActive: boolean;
    /** Agent notes is active here (its sentence would be news to no one). */
    notesActive: boolean;
    onOpenKnowledge: () => void;
    /** Open the Extensions tab. */
    onOpenExtensions: () => void;
  }

  let { knowledge, providerActive, notesActive, onOpenKnowledge, onOpenExtensions }: Props = $props();

  const lookup = $derived($knowledgeLookup);
  const waiting = $derived(lookup !== null ? waitingOnYou(lookup.k, lookup.idx, 2) : null);
  const changed = $derived.by(() => {
    if (lookup === null) return [];
    return whatChanged(lookup.idx, isoDay(Date.now()), 7)
      .flatMap((d) => d.entries)
      .slice(0, 3);
  });
  const next = $derived(knowledge?.left_off?.next.slice(0, 3) ?? []);
  const blocker = $derived(knowledge?.left_off?.blockers[0] ?? null);
  const hasContent = $derived(
    (waiting?.items.length ?? 0) > 0 || changed.length > 0 || next.length > 0 || blocker !== null,
  );

  function openEntry(ekey: string): void {
    focusKnowledgeEntry(ekey);
    onOpenKnowledge();
  }
</script>

<section class="stand" aria-labelledby="stand-title">
  <div class="shead">
    <span id="stand-title" class="lbl">Where things stand</span>
    {#if providerActive && knowledge?.provider}
      <span class="sub">from {knowledge.provider}</span>
      <button class="link" onclick={onOpenKnowledge}>open knowledge →</button>
    {/if}
  </div>

  {#if !providerActive}
    <p class="quiet">
      {#if !notesActive}Agent notes lets your agents leave each other findings and blockers on the Timeline.{" "}{/if}Mycelium
      lets this project remember what was learned and why. {notesActive ? "It is" : "Both are"} optional, in
      <button class="link inline" onclick={onOpenExtensions}>Extensions</button>.
    </p>
  {:else if knowledge === null || lookup === null}
    <p class="empty">loading…</p>
  {:else if !hasContent}
    <p class="empty">Nothing recorded this week — agents record findings, decisions and learnings as they work.</p>
  {:else}
    <div class="card" class:two={next.length > 0 || blocker !== null}>
      <div class="findings">
        {#if waiting !== null && waiting.items.length > 0}
          <div class="lbl small">Waiting on you</div>
          {#each waiting.items as w, i (i)}
            <button
              class="frow"
              onclick={() => (w.entry !== null ? openEntry(w.entry.ekey) : onOpenKnowledge())}
              title="open in Knowledge"
            >
              <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
              <span class="claim">{@html inlineMarkdown(w.text)}</span>
              <span class="status mono">{w.entry !== null ? qualifiedId(lookup.idx, w.entry) : w.sourceLabel}</span>
            </button>
          {/each}
        {/if}
        {#if changed.length > 0}
          <div class="lbl small">Recently recorded</div>
          {#each changed as e (e.ekey)}
            {@const st = stateLabel(e)}
            <button class="frow" onclick={() => openEntry(e.ekey)} title="open in Knowledge">
              <span class="status mono">{qualifiedId(lookup.idx, e)}</span>
              <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
              <span class="claim">{@html inlineMarkdown(e.title)}</span>
              {#if e.amends.length > 0}
                <span class="status warn">{e.amends[0].kind} {e.amends[0].id}</span>
              {:else if st !== null}
                <span class="status {st.tone}">{st.text}</span>
              {/if}
            </button>
          {/each}
        {/if}
      </div>
      {#if next.length > 0 || blocker !== null}
        <div class="next">
          {#if next.length > 0}
            <div class="lbl small">Next</div>
            {#each next as n, i (i)}
              <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
              <div class="nrow">{@html inlineMarkdown(n)}</div>
            {/each}
          {/if}
          {#if blocker !== null}
            <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
            <div class="blocked">Blocked: {@html inlineMarkdown(blocker)}</div>
          {/if}
        </div>
      {/if}
    </div>
  {/if}
</section>

<style>
  .stand {
    display: flex;
    flex-direction: column;
    gap: 10px;
    min-width: 0;
  }
  .shead {
    display: flex;
    align-items: baseline;
    gap: 10px;
  }
  .lbl {
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
    font-weight: 600;
  }
  .lbl.small {
    font-size: 10.5px;
    padding-bottom: 2px;
  }
  .findings .lbl.small:not(:first-child) {
    margin-top: 8px;
  }
  .sub {
    font-size: var(--text-xs);
    color: var(--muted);
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
  .link.inline {
    margin-left: 0;
    font-size: inherit;
  }

  /* Never in the way: body-size muted prose, a readable measure. */
  .quiet {
    margin: 0;
    max-width: 68ch;
    font-size: var(--text-sm);
    color: var(--muted);
    line-height: 1.55;
  }

  .empty {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--muted);
    line-height: 1.5;
  }

  .card {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 28px;
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 10px;
    padding: 14px 18px;
  }
  .card.two {
    grid-template-columns: minmax(0, 1.35fr) minmax(0, 1fr);
  }
  @container (max-width: 700px) {
    .card.two {
      grid-template-columns: minmax(0, 1fr);
    }
  }

  .findings {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }
  .frow {
    display: flex;
    gap: 10px;
    align-items: baseline;
    appearance: none;
    border: none;
    background: none;
    padding: 4px 6px;
    margin: 0 -6px;
    border-radius: 6px;
    font: inherit;
    font-size: var(--text-sm);
    color: var(--fg);
    text-align: left;
    cursor: pointer;
    min-width: 0;
  }
  .frow:hover {
    background: var(--row-hover);
  }
  .claim {
    flex: 1;
    min-width: 0;
    line-height: 1.45;
  }
  .claim :global(code) {
    font-family: var(--mono);
    font-size: 0.92em;
  }
  .status {
    flex: none;
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .status.mono {
    font-family: var(--mono);
  }
  .status.good {
    color: var(--accent);
  }
  .status.warn {
    color: var(--warn);
  }
  .status.bad {
    color: var(--err);
    font-weight: 600;
  }

  .next {
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: var(--text-sm);
    min-width: 0;
  }
  .nrow {
    line-height: 1.45;
  }
  .nrow :global(code),
  .blocked :global(code) {
    font-family: var(--mono);
    font-size: 0.92em;
  }
  .blocked {
    color: var(--warn);
    background: color-mix(in srgb, var(--warn) 9%, transparent);
    padding: 6px 10px;
    border-radius: 6px;
    margin-top: 2px;
    line-height: 1.45;
  }
</style>
