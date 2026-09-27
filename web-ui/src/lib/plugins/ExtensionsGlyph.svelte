<script lang="ts">
  /**
   * The Extensions glyph (glyph.ts) at its own pixel size — the one way the
   * pane tab, the rail row and the quick-open row draw it.
   */
  import { EXTENSIONS_GLYPH, EXTENSIONS_SIZE, EXTENSIONS_STROKE } from "./glyph";

  interface Props {
    /** The tab's hover name; omitted where a label says it already. */
    title?: string;
    class?: string;
  }

  let { title, class: cls = "" }: Props = $props();
</script>

<svg
  class="ext-glyph {cls}"
  viewBox="0 0 {EXTENSIONS_SIZE} {EXTENSIONS_SIZE}"
  width={EXTENSIONS_SIZE}
  height={EXTENSIONS_SIZE}
  aria-hidden="true"
>
  {#if title !== undefined}<title>{title}</title>{/if}
  <path
    d={EXTENSIONS_GLYPH}
    fill="none"
    stroke="currentColor"
    stroke-width={EXTENSIONS_STROKE}
    stroke-linecap="butt"
    stroke-linejoin="round"
  />
</svg>

<style>
  .ext-glyph {
    flex: none;
    display: block;
  }
</style>
