<script lang="ts">
  /**
   * The one-click "reference in agent" affordance for spots that are pointed
   * at, not selected: a notebook cell, a slide, a media moment. It builds the
   * selection at click time and sends it through the same handler as the
   * chord and the floating chip (parity principle). Disabled, with the
   * reason in its tooltip, when the workspace has no agent to receive it.
   */
  import { referenceNow, referenceTarget, type FileSelection } from "./reference";
  import { PINNED } from "./keys";

  interface Props {
    /** The selection to send, built when clicked (null sends nothing). */
    pick: () => FileSelection | null;
    /** What is pointed at ("cell 7", "slide 3", "0:12"), for the tooltip. */
    label: string;
    /** Visible text after the `@`; none makes a compact icon button. */
    text?: string;
    class?: string;
  }

  let { pick, label, text, class: cls = "" }: Props = $props();

  const target = $derived($referenceTarget);
  const owner = {};
</script>

<button
  class="ref-btn {cls}"
  class:icon={text === undefined}
  disabled={target === null}
  aria-label="reference {label} in agent"
  title={target === null
    ? "no agent session in this workspace — start one to reference"
    : `reference ${label} in ${target.name} (${PINNED.reference})`}
  onpointerdown={(e) => e.stopPropagation()}
  onclick={(e) => {
    e.stopPropagation();
    const sel = pick();
    if (sel !== null) referenceNow(owner, sel);
  }}
>
  <span class="at" aria-hidden="true">@</span>
  {#if text !== undefined}<span class="txt">{text}</span>{/if}
</button>

<style>
  .ref-btn {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    appearance: none;
    border: 1px solid var(--edge);
    background: color-mix(in srgb, var(--term-bg) 88%, transparent);
    color: var(--fg);
    font: inherit;
    font-family: var(--mono);
    font-size: var(--text-xs);
    line-height: 1;
    padding: 3px 7px;
    border-radius: 5px;
    cursor: pointer;
    white-space: nowrap;
    user-select: none;
    transition:
      background-color 0.1s ease,
      color 0.1s ease,
      border-color 0.1s ease,
      opacity 0.12s ease;
  }

  .ref-btn.icon {
    padding: 3px 5px;
  }

  .ref-btn:hover:enabled {
    background: var(--row-hover);
    border-color: color-mix(in srgb, var(--accent) 45%, var(--edge));
  }

  .ref-btn:focus-visible {
    outline: 2px solid var(--focus-ring);
    outline-offset: 1px;
  }

  .ref-btn:disabled {
    color: var(--muted);
    opacity: 0.7;
    cursor: default;
  }

  .at {
    color: var(--accent);
    font-weight: 600;
  }

  .ref-btn:disabled .at {
    color: var(--muted);
  }

  .txt {
    font-variant-numeric: tabular-nums;
  }

  @media (prefers-reduced-motion: reduce) {
    .ref-btn {
      transition: none;
    }
  }
</style>
