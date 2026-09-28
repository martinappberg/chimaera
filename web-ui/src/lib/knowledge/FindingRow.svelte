<script lang="ts">
  /**
   * One finding: id · claim · meta + badges · the confidence ladder · the
   * evidence strip (one mark per ledger row: filled supports, half refines,
   * ring contradicts), expanding to So what / Evidence / Open questions /
   * open in file. Everything shown is what the agent wrote; one-liners go
   * through inlineMarkdown, the body through the sanitized Markdown.
   */
  import Markdown from "../chat/Markdown.svelte";
  import { inlineMarkdown } from "../shared/inlineMarkdown";
  import type { Finding } from "../workspace/knowledge";
  import Ladder from "./Ladder.svelte";
  import { directionTone, ledgerCaption, type Tone } from "./model";

  interface Props {
    finding: Finding;
    topic: string;
    open: boolean;
    /** A recent status move from the Timeline ("now supported"), if any. */
    badge: { text: string; tone: Tone } | null;
    onToggle: () => void;
    /** Open the topic file (the user's correction path). */
    onOpenFile: () => void;
    /** The topic file's display path (workspace-relative). */
    filePath: string;
    visible: boolean;
  }

  let { finding, topic, open, badge, onToggle, onOpenFile, filePath, visible }: Props = $props();

  const caption = $derived(ledgerCaption(finding.ledger));
  const statusWord = $derived(finding.status === "unknown" ? "unrated" : finding.status);
</script>

<div class="finding" id="finding-{finding.id}">
  <button class="row" aria-expanded={open} onclick={onToggle}>
    <span class="id">{finding.id}</span>
    <span class="body">
      <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
      <span class="claim">{@html inlineMarkdown(finding.claim)}</span>
      <span class="meta">
        <span class="topic">{topic}</span>
        {#if finding.updated}<span>· {finding.updated}</span>{/if}
        {#if finding.recorded_by}<span>· recorded by <span class="mono">{finding.recorded_by.name}</span></span>{/if}
        {#if badge !== null}<span class="badge {badge.tone}">{badge.text}</span>{/if}
      </span>
    </span>
    <span class="conf">
      <Ladder status={finding.status} />
      <span class="word {finding.status}">{statusWord}</span>
    </span>
    <span class="strip">
      <span class="marks" aria-hidden="true">
        {#each finding.ledger as row, i (i)}
          <span class="mark {row.direction}" title="{row.date} · {row.run} · {row.direction}"></span>
        {/each}
      </span>
      <span class="caption">{caption}</span>
    </span>
  </button>

  {#if open}
    <div class="detail">
      {#if finding.implications}
        <div class="block">
          <div class="lbl">So what</div>
          <div class="md">
            <Markdown text={finding.implications} {visible} />
          </div>
        </div>
      {/if}
      {#if finding.ledger.length > 0}
        <div class="block">
          <div class="lbl">Evidence</div>
          <div class="ledger">
            {#each finding.ledger as row, i (i)}
              <div class="lrow">
                <span class="mono muted">{row.date}</span>
                <span class="mono muted">{row.run}</span>
                <span class="muted">{row.dataset}</span>
                <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
                <span class="result">{@html inlineMarkdown(row.result)}</span>
                <span class="dir {directionTone(row.direction)}">{row.direction}</span>
              </div>
            {/each}
          </div>
        </div>
      {/if}
      {#if finding.questions.length > 0}
        <div class="block">
          <div class="lbl">Open questions</div>
          {#each finding.questions as q, i (i)}
            <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
            <div class="q">{@html inlineMarkdown(q)}</div>
          {/each}
        </div>
      {/if}
      {#if finding.tags.length > 0}
        <div class="tags">{finding.tags.map((t) => `#${t}`).join("  ")}</div>
      {/if}
      <div class="foot">
        <button class="link mono" onclick={onOpenFile} title="open the file — the correction path">{filePath}</button>
        {#if finding.line > 0}<span class="muted">line {finding.line}</span>{/if}
      </div>
    </div>
  {/if}
</div>

<style>
  .finding {
    border-top: 1px solid var(--edge);
  }
  .finding:first-child {
    border-top: none;
  }
  .row {
    width: 100%;
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    color: var(--fg);
    text-align: left;
    cursor: pointer;
    display: grid;
    grid-template-columns: 58px minmax(0, 1fr) 132px 128px;
    column-gap: 16px;
    align-items: center;
    padding: 13px 18px;
    transition: background-color 0.12s ease;
  }
  .row:hover {
    background: var(--row-hover);
  }
  @container (max-width: 760px) {
    .row {
      grid-template-columns: 58px minmax(0, 1fr);
      row-gap: 8px;
    }
    .conf,
    .strip {
      grid-column: 2;
    }
  }
  .id {
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .body {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }
  .claim {
    font-size: var(--text-md);
    line-height: 1.4;
    overflow-wrap: anywhere;
  }
  .claim :global(code),
  .result :global(code),
  .q :global(code) {
    font-family: var(--mono);
    font-size: 0.92em;
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .topic {
    font-family: var(--mono);
  }
  .mono {
    font-family: var(--mono);
  }
  .muted {
    color: var(--muted);
  }
  .badge {
    padding: 1px 8px;
    border-radius: 999px;
    font-size: 11.5px;
    color: var(--muted);
    border: 1px solid var(--edge);
  }
  .badge.accent {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    color: var(--accent);
    border-color: transparent;
  }
  .badge.err {
    background: color-mix(in srgb, var(--err) 10%, transparent);
    color: var(--err);
    border-color: transparent;
  }

  .conf {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: var(--text-xs);
  }
  .word {
    color: var(--muted);
  }
  .word.supported {
    color: var(--accent);
  }
  .word.robust {
    color: var(--accent);
    font-weight: 600;
  }
  .word.contradicted {
    color: var(--err);
    font-weight: 600;
  }

  .strip {
    display: flex;
    flex-direction: column;
    gap: 5px;
    align-items: flex-start;
  }
  .marks {
    display: flex;
    gap: 4px;
    flex-wrap: wrap;
  }
  .mark {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    background: var(--accent);
  }
  .mark.refines {
    border: 2px solid var(--accent);
    background: linear-gradient(90deg, var(--accent) 50%, transparent 50%);
  }
  .mark.contradicts {
    border: 2px solid var(--err);
    background: transparent;
  }
  .mark.unknown {
    border: 2px solid var(--muted);
    background: transparent;
  }
  .caption {
    font-size: 11.5px;
    color: var(--muted);
  }

  .detail {
    margin: 0 18px 16px 92px;
    background: color-mix(in srgb, var(--fg) 3%, transparent);
    border-radius: 10px;
    padding: 14px 18px;
    display: flex;
    flex-direction: column;
    gap: 14px;
    animation: rise 0.18s ease;
  }
  @media (prefers-reduced-motion: reduce) {
    .detail {
      animation: none;
    }
  }
  @container (max-width: 760px) {
    .detail {
      margin-left: 18px;
    }
  }
  .block {
    display: flex;
    flex-direction: column;
    gap: 5px;
  }
  .lbl {
    font-size: 10.5px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
    font-weight: 600;
  }
  .md {
    font-size: var(--text-md);
    line-height: 1.5;
  }
  .md :global(p) {
    margin: 0 0 0.5em;
  }
  .md :global(p:last-child) {
    margin-bottom: 0;
  }
  .ledger {
    display: flex;
    flex-direction: column;
  }
  .lrow {
    display: grid;
    grid-template-columns: 82px 120px 110px minmax(0, 1fr) auto;
    column-gap: 12px;
    align-items: baseline;
    padding: 7px 0;
    border-bottom: 1px solid var(--edge);
    font-size: var(--text-sm);
  }
  .lrow:last-child {
    border-bottom: none;
  }
  .lrow .mono {
    font-size: 11.5px;
  }
  @container (max-width: 760px) {
    .lrow {
      grid-template-columns: minmax(0, 1fr) auto;
    }
    .lrow .muted {
      display: none;
    }
  }
  .dir {
    justify-self: end;
    font-size: 11.5px;
    padding: 1px 8px;
    border-radius: 999px;
    color: var(--muted);
    border: 1px solid var(--edge);
  }
  .dir.accent {
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    border-color: transparent;
  }
  .dir.err {
    color: var(--err);
    background: color-mix(in srgb, var(--err) 10%, transparent);
    border-color: transparent;
  }
  .q {
    font-size: var(--text-sm);
    line-height: 1.5;
  }
  .tags {
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    white-space: pre;
  }
  .foot {
    display: flex;
    gap: 14px;
    font-size: var(--text-xs);
    padding-top: 2px;
    align-items: baseline;
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
</style>
