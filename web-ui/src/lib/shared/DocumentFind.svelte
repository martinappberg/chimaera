<script lang="ts">
  import { tick, untrack } from "svelte";
  import { findTarget } from "./find";
  import { pageVisible } from "./visibility";
  import { domMatches, matchPainter, revealMatch } from "./domFind";
  import FindBar from "./FindBar.svelte";
  let { target, enabled = true }: { target: HTMLElement | null; enabled?: boolean } = $props();
  let open = $state(false);
  let query = $state("");
  let caseSensitive = $state(false);
  let ranges = $state.raw<Range[]>([]);
  let capped = $state(false);
  let current = $state(0);
  let bar = $state<FindBar>();
  let painter: ReturnType<typeof matchPainter> | null = null;
  let timer: ReturnType<typeof setTimeout> | undefined;
  function search(reveal = true) {
    if (!target) return;
    const result = domMatches(target, query, caseSensitive);
    ranges = result.ranges; capped = result.capped;
    current = Math.min(current, Math.max(0, ranges.length - 1));
    painter?.paint(ranges, current);
    if (reveal && ranges[current]) revealMatch(ranges[current], target);
  }
  function step(direction: 1 | -1) {
    if (!ranges.length || !target) return;
    current = (current + direction + ranges.length) % ranges.length;
    painter?.paint(ranges, current);
    revealMatch(ranges[current], target);
  }
  function close(restore = true) {
    open = false;
    painter?.destroy(); painter = null;
    // Ranges retain text nodes even if a later document refresh removes them.
    ranges = [];
    clearTimeout(timer);
    timer = undefined;
    target?.dispatchEvent(new CustomEvent("chimaera-find-visibility", { detail: false }));
    if (restore) target?.focus();
  }
  $effect(() => {
    const root = target;
    if (!root || !enabled) { close(false); return; }
    const registration = findTarget(root, (command) => {
      if (command === "open") {
        const selection = document.getSelection();
        if (selection?.anchorNode && root.contains(selection.anchorNode)) {
          const text = selection.toString();
          if (text && text.length <= 512 && !text.includes("\n")) query = text;
        }
        if (!painter) painter = matchPainter();
        open = true;
        root.dispatchEvent(new CustomEvent("chimaera-find-visibility", { detail: true }));
        void tick().then(() => { if (open && enabled) { bar?.focus(); search(); } });
        return true;
      }
      if (!open) return false;
      if (command === "close") close(); else step(command === "next" ? 1 : -1);
      return true;
    });
    const layer = root.closest(".layer");
    const parked = new MutationObserver(() => { if (layer?.hasAttribute("inert")) close(false); });
    if (layer) parked.observe(layer, { attributes: true, attributeFilter: ["inert"] });
    return () => { parked.disconnect(); registration.destroy(); close(false); };
  });
  $effect(() => {
    if (!open || !target || !$pageVisible) return;
    untrack(() => search(false));
    const observer = new MutationObserver(() => {
      if (timer === undefined) timer = setTimeout(() => { timer = undefined; search(false); }, 150);
    });
    observer.observe(target, {
      childList: true, subtree: true, characterData: true,
      attributes: true, attributeFilter: ["open", "hidden", "aria-hidden"],
    });
    return () => { observer.disconnect(); clearTimeout(timer); timer = undefined; };
  });
</script>

{#if open && enabled}
  <FindBar bind:this={bar} {query} {caseSensitive} scope="Find in document" canStep={ranges.length > 0}
    status={query === "" ? "" : ranges.length === 0 ? "No matches" : `${current + 1} of ${ranges.length}${capped ? "+" : ""}`}
    onQuery={(value) => { query = value; current = 0; search(); }}
    onCase={(value) => { caseSensitive = value; current = 0; search(); }} onStep={step} onClose={() => close()} />
  {#if capped}<span class="limit">Search limit reached; showing the first matches in this document.</span>{/if}
{/if}
<style>.limit { font-size: 11px; padding: 3px 10px; color: var(--fg-dim); }</style>
