<script lang="ts">
  /** A short word with a tone ("blocked", "corrected by F-177"). */
  import type { Tone } from "./tone";

  interface Props {
    text: string;
    tone?: Tone;
    title?: string;
  }

  let { text, tone = "neutral", title }: Props = $props();
</script>

<span class="badge {tone}" {title}>{text}</span>

<style>
  .badge {
    display: inline-block;
    font-size: 10.5px;
    font-weight: 500;
    line-height: 16px;
    border-radius: 4px;
    padding: 0 5px;
    white-space: nowrap;
    color: var(--muted);
    background: color-mix(in srgb, var(--fg) 7%, transparent);
  }
  .accent {
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 11%, transparent);
  }
  .good {
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 13%, transparent);
  }
  .warn {
    color: var(--warn);
    background: color-mix(in srgb, var(--warn) 14%, transparent);
  }
  .bad {
    color: var(--err);
    background: color-mix(in srgb, var(--err) 12%, transparent);
  }
</style>
