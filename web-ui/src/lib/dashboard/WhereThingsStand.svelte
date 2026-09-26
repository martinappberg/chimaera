<script lang="ts">
  /**
   * "Where things stand" — the top of Knowledge on the dashboard (design
   * §3): the contradiction first, then the strongest recent findings with
   * the confidence ladder, the handoff's next steps, and a warn-toned
   * blocker line. With no structured provider it is one quiet line and the
   * single action that fills it (attach mycelium). Never a curated summary —
   * everything here is what the agents recorded.
   */
  import { inlineMarkdown } from "../shared/inlineMarkdown";
  import type { Knowledge } from "../workspace/knowledge";
  import Ladder from "../knowledge/Ladder.svelte";
  import { whereThingsStand } from "../knowledge/model";

  interface Props {
    knowledge: Knowledge | null;
    /** A structured provider (mycelium) is active in this workspace. */
    providerActive: boolean;
    onOpenKnowledge: () => void;
    /** Open the "Use mycelium for Knowledge" sheet. */
    onAttach: () => void;
  }

  let { knowledge, providerActive, onOpenKnowledge, onAttach }: Props = $props();

  const picks = $derived(knowledge !== null ? whereThingsStand(knowledge, 3) : []);
  const next = $derived(knowledge?.left_off?.next.slice(0, 3) ?? []);
  const blocker = $derived(knowledge?.left_off?.blockers[0] ?? null);
  const hasContent = $derived(picks.length > 0 || next.length > 0 || blocker !== null);
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
    <p class="attach">
      Your agents can record findings and decisions as they work.
      <button class="link inline" onclick={onAttach}>Use mycelium →</button>
    </p>
  {:else if knowledge === null}
    <p class="empty">loading…</p>
  {:else if !hasContent}
    <p class="empty">Nothing recorded yet — agents record findings, decisions and learnings as they work.</p>
  {:else}
    <div class="card" class:two={next.length > 0 || blocker !== null}>
      {#if picks.length > 0}
        <div class="findings">
          {#each picks as p (p.finding.id)}
            <button class="frow" onclick={onOpenKnowledge} title="{p.finding.id} · {p.topic} — open in Knowledge">
              <Ladder status={p.finding.status} />
              <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
              <span class="claim">{@html inlineMarkdown(p.finding.claim)}</span>
              <span class="status {p.finding.status}">{p.finding.status}</span>
            </button>
          {/each}
        </div>
      {/if}
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
    margin-left: 6px;
    font-size: var(--text-sm);
  }

  .attach,
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
  .status.supported,
  .status.robust {
    color: var(--accent);
  }
  .status.contradicted {
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
