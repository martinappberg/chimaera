<script lang="ts">
  /**
   * Over a project view while its project moves between the user's computer
   * and their cloud: nothing underneath can be used until the machine that
   * takes it serves it, so the whole view shimmers with one sentence saying
   * what is going on. Self-gating on `projectMoving`, which only a project
   * view's placement reads ever set.
   */
  import { movingSentence, projectMoving } from "../net/projectMoving";
  import LoadingRows from "../shared/LoadingRows.svelte";

  const sentence = $derived($projectMoving === null ? null : movingSentence($projectMoving));
</script>

{#if sentence !== null}
  <div class="moving" role="status" aria-live="polite" aria-busy="true">
    <div class="rows" aria-hidden="true"><LoadingRows variant="transcript" rows={24} label={sentence} /></div>
    <p class="sentence">{sentence}</p>
  </div>
{/if}

<style>
  .moving {
    position: fixed;
    inset: 0;
    /* Under the elsewhere card (190) and the auth overlay (200): those
       still say what they say over a moving project. */
    z-index: 189;
    display: flex;
    align-items: center;
    justify-content: center;
    background: color-mix(in srgb, var(--bg) 95%, transparent);
  }
  /* The shimmer stands for the whole view: a centred column of rows. */
  .rows {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    justify-content: center;
    padding: 0 max(24px, calc((100vw - 720px) / 2));
    overflow: hidden;
  }
  .sentence {
    position: relative;
    margin: 0;
    padding: 10px 18px;
    border: 1px solid var(--edge);
    border-radius: 8px;
    background: var(--overlay-bg);
    box-shadow: 0 8px 28px rgba(0, 0, 0, 0.12);
    color: var(--fg);
    font-size: var(--text-md);
    line-height: 1.5;
    text-align: center;
  }
</style>
