<script lang="ts">
  /**
   * The mark at the head of a tool row: one line icon per kind of call, so
   * a list of rows reads by shape — what ran, what was read, what changed —
   * before any word is read. Drawn, not typed: a text glyph takes its
   * weight and size from whichever font has it.
   */
  let { kind, size = 14 }: { kind: string; size?: number } = $props();
</script>

<svg
  class="tool-glyph"
  viewBox="0 0 16 16"
  width={size}
  height={size}
  fill="none"
  stroke="currentColor"
  stroke-width="1.3"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
>
  {#if kind === "execute"}
    <rect x="1.5" y="2.5" width="13" height="11" rx="2" />
    <path d="m4.5 6 2 2-2 2M8.5 10h3" />
  {:else if kind === "read"}
    <path d="M4 2.5h5l3 3v7.5a.5.5 0 0 1-.5.5h-7.500a.5.5 0 0 1-.5-.5v-10a.5.5 0 0 1 .5-.5zM9 2.5v3h3M6 8.500h4M6 10.800h4" />
  {:else if kind === "edit"}
    <path d="m3 13 .6-2.700 7-7a1.100 1.100 0 0 1 1.600 0l.5.5a1.100 1.100 0 0 1 0 1.600l-7 7L3 13zM9.500 4.400l2.100 2.100" />
  {:else if kind === "search"}
    <circle cx="7" cy="7" r="3.800" />
    <path d="M9.900 9.900 13.200 13.200" />
  {:else if kind === "fetch"}
    <circle cx="8" cy="8" r="5.500" />
    <path d="M2.500 8h11M8 2.500c1.700 1.600 2.500 3.400 2.500 5.500S9.700 11.900 8 13.500C6.300 11.900 5.500 10.100 5.500 8S6.300 4.100 8 2.500z" />
  {:else if kind === "delete"}
    <path d="M3 4.500h10M6.500 4.500V3h3v1.500M4.500 4.500l.6 8.500h5.800l.6-8.500M6.800 7v3.500M9.200 7v3.500" />
  {:else if kind === "move"}
    <path d="M3 8h10M9.500 4.500 13 8l-3.500 3.500" />
  {:else if kind === "think"}
    <path d="M6 13h4M6.300 11c-1.400-.8-2.300-2.200-2.300-3.800a4 4 0 0 1 8 0c0 1.600-.9 3-2.300 3.800z" />
  {:else if kind === "agent"}
    <path d="M8 2.500v11M3.300 5.300l9.400 5.400M12.700 5.300l-9.400 5.400" />
  {:else}
    <path d="M6 2.500v3M10 2.500v3M4.500 5.500h7v2a3.500 3.500 0 0 1-7 0zM8 11v2.500" />
  {/if}
</svg>

<style>
  .tool-glyph {
    flex: none;
    display: block;
  }
</style>
