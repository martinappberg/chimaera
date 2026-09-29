<script lang="ts">
  /** A toned band that says one thing about what's below it: a leading word
   *  (`title`), then `text` or the children. */
  import type { Snippet } from "svelte";
  import type { Tone } from "./tone";

  interface Props {
    tone?: Tone;
    title?: string;
    text?: string;
    children?: Snippet;
  }

  let { tone = "neutral", title, text, children }: Props = $props();
</script>

<div class="callout {tone}" role="note">
  {#if title}<span class="ctitle">{title}</span>{/if}
  {#if text}<span class="ctext">{text}</span>{/if}
  {@render children?.()}
</div>

<style>
  .callout {
    padding: 8px 12px;
    border-radius: 8px;
    font-size: var(--text-sm);
    line-height: 1.45;
    display: flex;
    flex-wrap: wrap;
    gap: 4px 10px;
    align-items: baseline;
    min-width: 0;
    background: color-mix(in srgb, var(--fg) 5%, transparent);
  }
  .accent,
  .good {
    background: color-mix(in srgb, var(--accent) 11%, transparent);
  }
  .warn {
    background: color-mix(in srgb, var(--warn) 13%, transparent);
  }
  .bad {
    background: color-mix(in srgb, var(--err) 10%, transparent);
  }
  .ctitle {
    font-weight: 600;
  }
  .accent .ctitle,
  .good .ctitle {
    color: var(--accent);
  }
  .warn .ctitle {
    color: var(--warn);
  }
  .bad .ctitle {
    color: var(--err);
  }
  .ctext {
    overflow-wrap: anywhere;
  }
</style>
