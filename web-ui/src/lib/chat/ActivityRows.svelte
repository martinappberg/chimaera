<script lang="ts">
  import type { Snippet } from "svelte";

  /** The expanded body of an activity line (a tool group's calls, a fold's
   *  rows): indented under a thin left rule, clustered like the column. */
  let { children }: { children: Snippet } = $props();
</script>

<div class="rows">
  {@render children()}
</div>

<style>
  .rows {
    margin: 2px 0 6px 2px;
    padding-left: 10px;
    border-left: 2px solid color-mix(in srgb, var(--edge) 70%, transparent);
  }
  /* Nested activity lines cluster as they do in the column (ChatView's
     `.activity + .activity`). */
  .rows > :global(.activity + .activity) {
    margin-top: -2px;
  }
</style>
