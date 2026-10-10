<script lang="ts">
  import type { Snippet } from "svelte";
  let { label, children }: { label: string; children: Snippet } = $props();
  let details: HTMLDetailsElement;
  let trigger: HTMLElement;
  let open = $state(false);
  function dismiss(event: MouseEvent): void {
    if (details && event.target instanceof Node && !details.contains(event.target)) details.open = false;
  }
  function escape(event: KeyboardEvent): void {
    if (event.key !== "Escape" || !details) return;
    event.preventDefault(); event.stopPropagation(); details.open = false; trigger.focus();
  }
  // Outside-click and Escape listeners exist only while this menu is open: a
  // Home full of rows would otherwise run every row's handlers on every click
  // and key anywhere in the window. `toggle` fires after the opening click has
  // finished dispatching, so that click never dismisses its own menu.
  // The list is position:fixed so a scrolling ancestor (Home's workspace list)
  // cannot clip it; it is placed from the trigger on open, flips above the
  // trigger when it would run off the bottom. A scroll of a box holding the
  // row moves the list with its trigger, and closes it only once the trigger
  // has left the window: a scroll that layout causes (a scroller clamping when
  // a line elsewhere on Home comes or goes) must not take the menu away from
  // under the pointer. Scrolls anywhere else (another list) leave it alone.
  let list: HTMLElement;
  function place(): void {
    const t = trigger.getBoundingClientRect();
    const h = list.offsetHeight;
    const below = t.bottom + 4;
    const top = below + h > window.innerHeight - 8 && t.top - 4 - h > 8 ? t.top - 4 - h : below;
    list.style.top = `${top}px`;
    list.style.right = `${Math.max(8, window.innerWidth - t.right)}px`;
  }
  function close(): void {
    if (details) details.open = false;
  }
  function follow(event: Event): void {
    if (!details?.open) return;
    const source = event.target;
    if (source !== document && !(source instanceof Node && source.contains(details))) return;
    const t = trigger.getBoundingClientRect();
    if (t.bottom < 0 || t.top > window.innerHeight) close();
    else place();
  }
  $effect(() => {
    if (!open) return;
    place();
    window.addEventListener("click", dismiss);
    window.addEventListener("keydown", escape);
    window.addEventListener("scroll", follow, true);
    window.addEventListener("resize", close);
    return () => {
      window.removeEventListener("click", dismiss);
      window.removeEventListener("keydown", escape);
      window.removeEventListener("scroll", follow, true);
      window.removeEventListener("resize", close);
    };
  });
</script>

<details class="actions" bind:this={details} ontoggle={() => (open = details?.open ?? false)} onfocusout={(event) => {
  if (details && event.relatedTarget instanceof Node && !details.contains(event.relatedTarget)) details.open = false;
}}>
  <summary bind:this={trigger} aria-label={label} title={label}>
    <svg viewBox="0 0 18 18" width="18" height="18" aria-hidden="true"><circle cx="4" cy="9" r="1.2" fill="currentColor" /><circle cx="9" cy="9" r="1.2" fill="currentColor" /><circle cx="14" cy="9" r="1.2" fill="currentColor" /></svg>
  </summary>
  <!-- Native buttons preserve ordinary tab navigation; this is a disclosure,
       not an ARIA menu with a second keyboard interaction model. -->
  <div class="action-list" bind:this={list} onclick={(event) => {
    if (details && event.target instanceof Element && event.target.closest("button")) details.open = false;
  }} role="presentation">{@render children()}</div>
</details>

<style>
  .actions { position: relative; flex: none; }
  summary { display: flex; align-items: center; justify-content: center; width: 32px; height: 32px; border-radius: 6px; color: var(--muted); list-style: none; cursor: pointer; }
  summary::-webkit-details-marker { display: none; }
  summary:hover, .actions[open] summary { color: var(--fg); background: var(--row-active); }
  summary:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 2px; }
  @media (pointer: coarse), (max-width: 700px) { summary { width: 40px; height: 40px; } .action-list :global(button) { min-height: 40px; } }
  .action-list { position: fixed; z-index: 30; min-width: 190px; padding: 5px; border: 1px solid var(--edge); border-radius: 8px; background: var(--overlay-bg); box-shadow: 0 8px 28px color-mix(in srgb, var(--fg) 12%, transparent); }
  .action-list :global(button) { display: block; visibility: visible; width: 100%; margin: 0; padding: 8px 10px; border: 0; border-radius: 4px; background: transparent; color: var(--fg); text-align: left; white-space: nowrap; font: inherit; font-size: var(--text-sm); cursor: pointer; }
  .action-list :global(button:hover) { background: var(--row-hover); }
  .action-list :global(button.stop) { color: var(--warn); }
</style>
