<script lang="ts">
  import Chevron from "../shared/Chevron.svelte";
  import { copyText } from "../shared/clipboard";
  import { failureText } from "./failureText";

  /**
   * Something failed, as one row in the transcript's own voice: at the rows'
   * edge with a small error mark, a short headline, and — when the headline
   * left something out — the whole text one click away in a framed panel you
   * can copy from. The same place and size as the "Stopped" line: the mark
   * carries the state, the words stay quiet.
   */
  interface Props {
    /** What ended, said first ("Turn failed"); null for a plain error line. */
    lead?: string | null;
    /** The failure's own words, raw (agent output: shown as text only). */
    text: string;
    /** Absolute transcript index + stable uid, for ChatView's scroll anchor;
     *  omitted outside the transcript (the fatal banner). */
    sourceIndex?: number;
    sourceUid?: number;
  }

  let { lead = null, text, sourceIndex, sourceUid }: Props = $props();

  const shaped = $derived(failureText(text));
  let open = $state(false);

  let copied = $state(false);
  let copiedTimer: ReturnType<typeof setTimeout> | null = null;
  function copyDetail() {
    const detail = shaped.detail;
    if (detail === null) return;
    void copyText(detail).then((ok) => {
      if (!ok) return;
      copied = true;
      if (copiedTimer !== null) clearTimeout(copiedTimer);
      copiedTimer = setTimeout(() => {
        copiedTimer = null;
        copied = false;
      }, 1400);
    });
  }
  $effect(() => () => {
    if (copiedTimer !== null) clearTimeout(copiedTimer);
  });
</script>

{#snippet words()}
  <span class="mark" aria-hidden="true"></span>
  <span class="words">
    {#if lead !== null}<span class="lead">{lead}{#if shaped.headline !== ""}<span class="sep" aria-hidden="true">·</span>{/if}</span>{/if}
    {#if shaped.headline !== ""}<span class="headline" class:first={lead === null}>{shaped.headline}</span>{/if}
  </span>
{/snippet}

<div class="failure" data-block-index={sourceIndex} data-block-uid={sourceUid}>
  {#if shaped.detail !== null}
    <button
      class="line opens"
      aria-expanded={open}
      title={open ? "hide the full error" : "show the full error"}
      onclick={() => (open = !open)}
    >
      {@render words()}
      <Chevron {open} />
    </button>
    {#if open}
      <div class="panel-wrap">
        <pre class="panel quiet-scrollbar">{shaped.detail}</pre>
        <button
          class="copy"
          class:copied
          aria-label={copied ? "copied" : "copy the full error"}
          title={copied ? "copied" : "copy the full error"}
          onclick={copyDetail}
        >
          {#if copied}
            <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
              <path d="M3.5 8.5l3 3 6-6.5" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" />
            </svg>
          {:else}
            <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
              <rect x="6" y="6" width="7.5" height="7.5" rx="1.5" fill="none" stroke="currentColor" stroke-width="1.5" />
              <path
                d="M4 10h-.5A1.5 1.5 0 0 1 2 8.5v-5A1.5 1.5 0 0 1 3.5 2h5A1.5 1.5 0 0 1 10 3.5V4"
                fill="none"
                stroke="currentColor"
                stroke-width="1.5"
                stroke-linecap="round"
              />
            </svg>
          {/if}
        </button>
      </div>
    {/if}
  {:else}
    <div class="line">{@render words()}</div>
  {/if}
</div>

<style>
  .failure {
    container-type: inline-size;
    padding: 4px 0;
    font-size: var(--text-sm);
  }
  .line {
    display: flex;
    align-items: baseline;
    gap: 7px;
    max-width: 100%;
    color: var(--muted);
    line-height: 1.45;
    text-align: left;
  }
  /* A line that opens is a button the width of its words, its hover tint
     reaching past the mark by the same 6px every activity line uses. */
  .line.opens {
    width: fit-content;
    margin: -2px 0 -2px -6px;
    padding: 2px 6px;
    background: none;
    border: none;
    border-radius: 6px;
    font: inherit;
    cursor: pointer;
    transition: background-color 0.12s ease;
  }
  .line.opens:hover,
  .line.opens:focus-visible {
    background: var(--hover-tint);
  }
  .line.opens :global(.chev) {
    align-self: center;
    opacity: 0.55;
  }
  .line.opens:hover :global(.chev) {
    opacity: 1;
  }
  /* The stop line's square, in the error colour and round: the one red
     thing on the row. Sits on the first line's centre, not the baseline. */
  .mark {
    flex: none;
    align-self: flex-start;
    width: 7px;
    height: 7px;
    margin-top: calc(0.725em - 3.5px);
    border-radius: 50%;
    background: var(--err);
  }
  .lead,
  .headline.first {
    color: var(--fg);
  }
  /* Lead and headline share one box; on a row too narrow to give the
     headline room beside the lead, it moves under the lead (not under the
     mark) instead of shrinking away. */
  .words {
    flex: 0 1 auto;
    min-width: 0;
    display: flex;
    align-items: baseline;
    column-gap: 7px;
  }
  /* Keep in step with STACK_BELOW_CHARS (failureText.ts). */
  @container (max-width: 240px) {
    .words {
      flex-direction: column;
      align-items: flex-start;
    }
    .words > .headline {
      max-width: 100%;
    }
    .sep {
      display: none;
    }
  }
  .lead {
    flex: none;
    white-space: nowrap;
  }
  .sep {
    margin-left: 7px;
    color: color-mix(in srgb, var(--muted) 60%, transparent);
  }
  .headline {
    flex: 0 1 auto;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  /* A clipped headline stays one line; the panel has the rest. */
  .opens .headline {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .panel-wrap {
    position: relative;
    margin: 6px 0 4px 14px;
  }
  .panel {
    margin: 0;
    max-height: 240px;
    overflow: auto;
    padding: 8px 10px;
    border: 1px solid color-mix(in srgb, var(--edge) 80%, transparent);
    border-radius: 7px;
    background: color-mix(in srgb, var(--fg) 2.5%, transparent);
    color: var(--muted);
    font-family: var(--mono, monospace);
    font-size: var(--text-xs);
    line-height: 1.5;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    scrollbar-width: thin;
    scrollbar-color: color-mix(in srgb, var(--fg) 22%, transparent) transparent;
  }
  .copy {
    position: absolute;
    top: 6px;
    right: 6px;
    display: inline-flex;
    padding: 4px;
    background: color-mix(in srgb, var(--bg) 82%, transparent);
    border: 1px solid var(--edge);
    border-radius: 5px;
    color: var(--muted);
    cursor: pointer;
    opacity: 0;
    transition:
      opacity 0.12s ease,
      color 0.12s ease;
  }
  .panel-wrap:hover .copy,
  .copy:focus-visible,
  .copy.copied {
    opacity: 1;
  }
  @media (hover: none) {
    .copy {
      opacity: 1;
    }
  }
  .copy:hover,
  .copy.copied {
    color: var(--accent);
  }
  @media (prefers-reduced-motion: reduce) {
    .line.opens,
    .copy {
      transition: none;
    }
  }
</style>
