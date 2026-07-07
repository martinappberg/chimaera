<script lang="ts">
  /**
   * The floating "reference in agent" affordance shown near a selection's end
   * in file views (context bridge). Quiet pill; disabled (with an explanatory
   * tooltip) when the workspace has no agent session to receive the
   * reference. Clicking never disturbs the selection (pointerdown is eaten)
   * and funnels through the same handler as the chord (parity principle).
   */
  import { referenceTarget, requestReference } from "./reference";
  import { KEYS } from "./keys";

  interface Props {
    /** Position within the nearest positioned ancestor, px. */
    x: number;
    y: number;
  }

  let { x, y }: Props = $props();

  const target = $derived($referenceTarget);
</script>

<button
  class="ref-chip"
  style:left="{x}px"
  style:top="{y}px"
  disabled={target === null}
  title={target === null
    ? "no agent session in this workspace — start one to reference"
    : `reference in ${target.name} (${KEYS.reference})`}
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
  <span class="at" aria-hidden="true">@</span>
  reference in agent
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
