<script lang="ts">
  /**
   * What the agents are told — AGENTS.md, CLAUDE.md, a plugin's protocol
   * file, the agents' own memory — as one row of links. It answers "what
   * are the agents told?" whether or not a knowledge plugin is on, so it
   * lives here rather than in Knowledge (which exists only with one).
   */
  import type { GuidanceFile } from "../workspace/knowledge";

  interface Props {
    guidance: GuidanceFile[];
    onOpen: (path: string) => void;
  }

  let { guidance, onOpen }: Props = $props();
</script>

{#if guidance.length > 0}
  <section class="guide" aria-labelledby="guide-title">
    <span id="guide-title" class="lbl">What the agents are told</span>
    <div class="links">
      {#each guidance as g (g.path)}
        <button class="g" onclick={() => onOpen(g.path)} title={g.description ? `${g.path} — ${g.description}` : g.path}>
          {g.label}
        </button>
      {/each}
    </div>
  </section>
{/if}

<style>
  .guide {
    display: flex;
    align-items: baseline;
    flex-wrap: wrap;
    gap: 6px 14px;
    min-width: 0;
  }
  .lbl {
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
    font-weight: 600;
  }
  .links {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    min-width: 0;
  }
  .g {
    appearance: none;
    border: 1px solid var(--edge);
    background: var(--overlay-bg);
    color: var(--fg);
    border-radius: 6px;
    padding: 2px 9px;
    font-family: var(--mono);
    font-size: 11.5px;
    cursor: pointer;
    white-space: nowrap;
  }
  .g:hover {
    border-color: var(--accent);
    color: var(--accent);
  }
</style>
