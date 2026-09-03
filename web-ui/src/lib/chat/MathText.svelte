<script lang="ts">
  import { safeMathHtml } from "./math";

  interface Props {
    source: string;
    display?: boolean;
  }

  let { source, display = false }: Props = $props();
  // User text is local input, but replayed transcript content still crosses a
  // wire/storage boundary. Keep it under the same sanitizer as agent math.
  const html = $derived(safeMathHtml(source, display));
</script>

<span class="math-text" class:display>{@html html}</span>

<style>
  /* Equation typography (.katex) is global — app.css. Display math scrolls
     within the bubble instead of widening it. */
  .math-text {
    color: inherit;
  }
  .math-text.display {
    display: block;
    max-width: 100%;
    overflow-x: auto;
    overflow-y: hidden;
    margin: 0.45em 0;
    padding: 0.08em 0;
  }
</style>
