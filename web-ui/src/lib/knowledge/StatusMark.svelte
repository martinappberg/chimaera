<script lang="ts">
  /**
   * A status exactly as the agent wrote it. When it starts with one of the
   * provider's status words, that word's ladder glyph (●●○, ✕) goes beside
   * it — the provider's scale, never ours; anything else is just the words.
   * Nothing renders for no status.
   */
  import type { KnowledgeLabels } from "../workspace/knowledge";
  import { statusWord, toneOf } from "./overview";

  interface Props {
    stated: string;
    labels: KnowledgeLabels;
    /** Only the first word (lists); the reader shows it all. */
    short?: boolean;
  }

  let { stated, labels, short = false }: Props = $props();

  const word = $derived(statusWord(labels, stated));
  const tone = $derived(word !== null ? toneOf(word.tone) : "neutral");
  const text = $derived.by(() => {
    const t = stated.trim();
    if (!short) return t;
    const first = t.match(/^[^\s(.,;:—–-]+/)?.[0] ?? t;
    return first.length > 24 ? `${first.slice(0, 24)}…` : first;
  });
</script>

{#if stated.trim() !== ""}
  <span class="status {tone}" title={short ? stated : undefined}>
    {#if word !== null && (word.rank > 0 || tone === "bad")}
      <span class="glyph" aria-hidden="true">
        {#if word.rank === 0}✕{:else}{#each [1, 2, 3] as i (i)}<i class:on={i <= word.rank}></i>{/each}{/if}
      </span>
    {/if}
    <span class="word">{text}</span>
  </span>
{/if}

<style>
  .status {
    display: inline-flex;
    align-items: baseline;
    gap: 6px;
    min-width: 0;
    color: var(--fg);
  }
  .word {
    overflow-wrap: anywhere;
  }
  .glyph {
    display: inline-flex;
    gap: 2px;
    align-self: center;
    font-size: 11px;
    line-height: 1;
  }
  .glyph i {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    box-shadow: inset 0 0 0 1.2px color-mix(in srgb, var(--muted) 70%, transparent);
  }
  .glyph i.on {
    background: var(--accent);
    box-shadow: none;
  }
  .bad {
    color: var(--err);
  }
  .bad .glyph {
    color: var(--err);
  }
  .warn .glyph i.on {
    background: var(--warn);
  }
</style>
