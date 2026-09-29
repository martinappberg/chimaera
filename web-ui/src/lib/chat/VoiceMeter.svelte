<script lang="ts">
  interface Props {
    /** The last few 0..1 input levels, oldest first. */
    levels: readonly number[];
    /** Hearing now (else the bars rest, e.g. while the mic starts or the
     *  last words are transcribed). */
    listening: boolean;
    /** The input recording, as the system names it ("" until it opens). */
    device: string;
  }

  let { levels, listening, device }: Props = $props();
</script>

<!-- The live waveform beside the stop button: each bar is one recent 100 ms
     level. A flat red row while you talk means the mic hears nothing. -->
<span
  class="meter"
  class:listening
  role="img"
  aria-label={listening ? "listening" : "transcribing"}
  title={device ? `Recording from ${device}` : undefined}
>
  {#each levels as level, i (i)}
    <span class="bar" style:--level={listening ? level : 0}></span>
  {/each}
</span>

<style>
  .meter {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    height: 16px;
  }
  /* Transform only, so the bars composite rather than repaint. */
  .bar {
    width: 3px;
    height: 16px;
    border-radius: 2px;
    background: var(--muted);
    opacity: 0.55;
    transform: scaleY(calc(0.2 + var(--level, 0) * 0.8));
    transition: transform 90ms linear;
  }
  .listening .bar {
    background: var(--err);
    opacity: 1;
  }
  @media (prefers-reduced-motion: reduce) {
    .bar {
      transition: none;
    }
  }
</style>
