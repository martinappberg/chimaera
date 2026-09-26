<script lang="ts">
  /**
   * mycelium's confidence ladder as one glyph: ●○○ preliminary · ●●○
   * supported · ●●● robust · ✕ contradicted. Derived by mycelium from the
   * evidence ledger, never by us; explained once by the legend beside the
   * Knowledge list. Colour is paired with the status word by the caller.
   */
  import { ladderFill } from "./model";

  interface Props {
    status: string;
    /** Glyph size in px (the dots are drawn at this height). */
    size?: number;
  }

  let { status, size = 9 }: Props = $props();

  const fill = $derived(ladderFill(status));
</script>

{#if fill === null}
  <span class="ladder x" style:font-size="{size + 2}px" aria-label="contradicted" role="img">✕</span>
{:else}
  <span class="ladder" style:font-size="{size}px" aria-label={status} role="img">
    {#each [0, 1, 2] as i (i)}<span class="dot" class:on={i < fill}>●</span>{/each}
  </span>
{/if}

<style>
  .ladder {
    flex: none;
    display: inline-flex;
    letter-spacing: 1.5px;
    line-height: 1;
    width: 30px;
    color: var(--accent);
  }
  .ladder.x {
    color: var(--err);
    font-weight: 700;
    letter-spacing: 0;
  }
  .dot {
    color: var(--edge);
  }
  .dot.on {
    color: var(--accent);
  }
</style>
