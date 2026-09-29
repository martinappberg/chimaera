<script lang="ts">
  /**
   * The floating "reference in agent" affordance shown near a selection's end
   * in file views and chat transcripts (context bridge). Quiet pill; disabled
   * (with an explanatory tooltip) when there is no agent session to receive
   * the reference. Clicking never disturbs the selection (pointerdown is
   * eaten) and funnels through the same handler as the chord (parity
   * principle).
   */
  import { referenceTarget, requestReference } from "./reference";
  import { PINNED } from "./keys";

  interface Props {
    /** Position within the nearest positioned ancestor, px. */
    x: number;
    y: number;
    /** What is pointed at ("p. 3 region", "rows 5–9"), for the tooltip. */
    label?: string;
    /** A chat transcript's selection: quoted into that chat's own reply,
     *  and worded as such. */
    quote?: boolean;
  }

  let { x, y, label, quote = false }: Props = $props();

  const target = $derived($referenceTarget);
  const title = $derived.by(() => {
    if (quote) {
      return target === null
        ? "this chat's agent isn't running — nothing to reply to"
        : `quote the selection in your reply (${PINNED.reference})`;
    }
    return target === null
      ? "no agent session in this workspace — start one to reference"
      : `reference ${label !== undefined ? `${label} ` : ""}in ${target.name} (${PINNED.reference})`;
  });
</script>

<button
  class="ref-chip"
  style:left="{x}px"
  style:top="{y}px"
  disabled={target === null}
  {title}
  onpointerdown={(e) => {
    // Keep the selection alive: the press must never collapse it or start
    // a drag; the click alone acts.
    e.preventDefault();
    e.stopPropagation();
  }}
  onclick={(e) => {
    e.stopPropagation();
    requestReference();
  }}
>
  {#if quote}
    <span class="at" aria-hidden="true">&gt;</span>
    quote in reply
  {:else}
    <span class="at" aria-hidden="true">@</span>
    reference in agent
  {/if}
</button>

<style>
  .ref-chip {
    position: absolute;
    z-index: 12;
    display: flex;
    align-items: center;
    gap: 5px;
    appearance: none;
    border: 1px solid var(--edge);
    background: var(--overlay-bg, var(--term-bg));
    color: var(--fg);
    font: inherit;
    font-size: var(--text-xs);
    font-family: var(--mono);
    line-height: 1;
    padding: 4px 8px;
    border-radius: 6px;
    box-shadow: 0 3px 12px rgba(0, 0, 0, 0.16);
    cursor: pointer;
    white-space: nowrap;
    user-select: none;
    animation: ref-chip-in 0.1s ease-out;
    transition:
      background-color 0.1s ease,
      color 0.1s ease,
      opacity 0.1s ease;
  }

  @keyframes ref-chip-in {
    from {
      opacity: 0;
      transform: translateY(2px);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .ref-chip {
      animation: none;
    }
  }

  .ref-chip:hover:enabled {
    background: var(--row-hover);
  }

  .ref-chip:active:enabled {
    transform: translateY(0.5px);
  }

  .ref-chip:focus-visible {
    outline: 2px solid var(--focus-ring);
    outline-offset: 1px;
  }

  .ref-chip:disabled {
    color: var(--muted);
    opacity: 0.75;
    cursor: default;
  }

  .at {
    color: var(--accent);
    font-weight: 600;
  }

  .ref-chip:disabled .at {
    color: var(--muted);
  }
</style>
