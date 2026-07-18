<script lang="ts">
  import DOMPurify from "dompurify";
  import { renderMath } from "./math";

  interface Props {
    source: string;
    display?: boolean;
  }

  let { source, display = false }: Props = $props();
  // User text is local input, but replayed transcript content still crosses a
  // wire/storage boundary. Keep it under the same sanitizer as agent math.
  const html = $derived(
    DOMPurify.sanitize(renderMath(source, display), {
      FORBID_TAGS: ["style"],
      FORBID_ATTR: ["style"],
    }),
  );
</script>

<span class="math-text" class:display>{@html html}</span>

<style>
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
  .math-text :global(.katex) {
    color: inherit;
    font-size: 1.02em;
  }
  .math-text.display :global(.katex-display) {
    display: block;
    width: max-content;
    min-width: 100%;
    margin: 0;
  }
</style>
