<script lang="ts">
  /**
   * A file (or file-like thing: a job, a commit) something names. With
   * `onclick` it opens; without, it is a plain label (a path that doesn't
   * resolve here, a Slurm job id).
   */
  interface Props {
    /** The path or id, as written. */
    title: string;
    /** What it is ("script", "data", "job"). */
    subtitle?: string;
    /** Where it resolved to (a tooltip). */
    text?: string;
    onclick?: () => void;
  }

  let { title, subtitle, text, onclick }: Props = $props();
</script>

{#if onclick}
  <button class="fcard open" {onclick} title={text ?? title}>
    {#if subtitle}<span class="k">{subtitle}</span>{/if}<span class="t">{title}</span>
  </button>
{:else}
  <span class="fcard" title={text ?? title}>
    {#if subtitle}<span class="k">{subtitle}</span>{/if}<span class="t">{title}</span>
  </span>
{/if}

<style>
  .fcard {
    display: inline-flex;
    align-items: baseline;
    gap: 5px;
    max-width: 100%;
    font-family: var(--mono);
    font-size: 11.5px;
    line-height: 18px;
    border: 1px solid var(--edge);
    background: var(--overlay-bg);
    color: var(--fg);
    border-radius: 5px;
    padding: 1px 6px;
    min-width: 0;
  }
  .t {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }
  .k {
    color: var(--muted);
    font-family: var(--ui-font);
    font-size: 10.5px;
    flex: none;
  }
  .open {
    cursor: pointer;
    font: inherit;
    font-family: var(--mono);
    font-size: 11.5px;
  }
  .open:hover {
    border-color: var(--accent);
    color: var(--accent);
  }
</style>
