<script lang="ts">
  /**
   * The singleton context-menu surface (see contextMenu.svelte.ts). Fixed at
   * the pointer, clamped to the viewport (flips above the cursor when it
   * would overflow the bottom edge), full keyboard nav with a roving active
   * row shared by mouse hover. z-index sits below modals (FolderPicker is
   * 100) — a dialog always wins over a lingering menu.
   */
  import { tick } from "svelte";
  import {
    contextMenu,
    type ContextMenuItem,
  } from "./contextMenu.svelte";
  import { dismiss } from "./dismiss";

  let menuEl = $state<HTMLElement | null>(null);
  let left = $state(0);
  let top = $state(0);
  let activeIndex = $state(-1);

  const selectable = $derived(
    contextMenu.items.reduce<number[]>((acc, entry, i) => {
      if (entry !== "separator" && entry.disabled !== true) acc.push(i);
      return acc;
    }, []),
  );

  /** A pick list (some row carries `checked`) grows a mark gutter on every
   *  row, so checked and unchecked labels stay aligned. */
  const hasMarks = $derived(
    contextMenu.items.some((entry) => entry !== "separator" && entry.checked !== undefined),
  );

  // Land at the pointer immediately (never a flash at the previous spot),
  // then clamp once the size is measurable.
  $effect(() => {
    if (!contextMenu.open) return;
    const px = contextMenu.x;
    const py = contextMenu.y;
    left = px;
    top = py;
    activeIndex = -1;
    void tick().then(() => {
      const el = menuEl;
      if (el === null) return;
      const rect = el.getBoundingClientRect();
      left = Math.max(4, Math.min(px, window.innerWidth - rect.width - 4));
      top = py + rect.height > window.innerHeight - 4 ? Math.max(4, py - rect.height) : py;
      el.focus();
      const checked = contextMenu.items.findIndex((e) => e !== "separator" && e.checked === true);
      if (checked >= 0) revealRow(checked);
    });
  });

  // A resize or window blur invalidates the anchor point — just close.
  $effect(() => {
    if (!contextMenu.open) return;
    const close = () => contextMenu.close();
    window.addEventListener("resize", close);
    window.addEventListener("blur", close);
    return () => {
      window.removeEventListener("resize", close);
      window.removeEventListener("blur", close);
    };
  });

  function select(item: ContextMenuItem): void {
    if (item.disabled === true) return;
    contextMenu.close();
    item.onSelect();
  }

  function move(delta: number): void {
    if (selectable.length === 0) return;
    const pos = selectable.indexOf(activeIndex);
    const next =
      pos < 0
        ? delta > 0
          ? 0
          : selectable.length - 1
        : (pos + delta + selectable.length) % selectable.length;
    activeIndex = selectable[next];
  }

  /** Scroll row `i` into the menu's own view (the menu scrolls when a pick
   *  list outgrows the viewport). Own math: the menu is the scroller and
   *  scrollIntoView would also move whatever sits behind it. */
  function revealRow(i: number): void {
    const el = menuEl;
    const row = el?.querySelector<HTMLElement>(`[data-row="${i}"]`);
    if (el == null || row == null) return;
    const top = row.offsetTop;
    const bottom = top + row.offsetHeight;
    if (top < el.scrollTop) el.scrollTop = top;
    else if (bottom > el.scrollTop + el.clientHeight) el.scrollTop = bottom - el.clientHeight;
  }

  // The roving row follows the keyboard into view; on open, the checked row
  // of a pick list is shown without a keypress.
  $effect(() => {
    const i = activeIndex;
    if (i >= 0) revealRow(i);
  });

  function onKeydown(e: KeyboardEvent): void {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      move(1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      move(-1);
    } else if (e.key === "Home") {
      e.preventDefault();
      activeIndex = selectable[0] ?? -1;
    } else if (e.key === "End") {
      e.preventDefault();
      activeIndex = selectable.at(-1) ?? -1;
    } else if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      const entry = contextMenu.items[activeIndex];
      if (entry !== undefined && entry !== "separator") select(entry);
    }
    // Escape is handled by the dismiss action.
  }
</script>

{#if contextMenu.open}
  <div
    class="ctx overlay-surface"
    role="menu"
    tabindex="-1"
    bind:this={menuEl}
    style:left={`${left}px`}
    style:top={`${top}px`}
    use:dismiss={{ enabled: contextMenu.open, onDismiss: () => contextMenu.close() }}
    onkeydown={onKeydown}
    oncontextmenu={(e) => e.preventDefault()}
  >
    {#each contextMenu.items as entry, i (i)}
      {#if entry === "separator"}
        <div class="ctx-sep" role="separator"></div>
      {:else}
        <button
          class="overlay-row ctx-row"
          class:danger={entry.danger}
          class:active={i === activeIndex}
          role="menuitem"
          data-row={i}
          disabled={entry.disabled}
          title={entry.disabled === true ? entry.hint : undefined}
          onclick={() => select(entry)}
          onpointerenter={() => {
            if (entry.disabled !== true) activeIndex = i;
          }}
        >{#if hasMarks}<span class="ctx-mark" aria-hidden="true">{entry.checked === true ? "✓" : ""}</span>{/if}{entry.label}</button>
      {/if}
    {/each}
  </div>
{/if}

<style>
  .ctx {
    position: fixed;
    z-index: 90;
    min-width: 172px;
    outline: none;
    /* A long pick list (every tab of a crowded pane) scrolls inside the
       viewport instead of running off its bottom edge. */
    max-height: calc(100vh - 8px);
    overflow-y: auto;
    scrollbar-width: thin;
  }

  .ctx-row {
    display: block;
    white-space: nowrap;
  }

  /* The pick-list gutter: a fixed slot so a check never shifts the label. */
  .ctx-mark {
    display: inline-block;
    width: 14px;
    color: var(--accent);
    font-weight: 600;
  }

  /* The roving active row: keyboard and hover share one highlight. */
  .ctx-row.active {
    background: var(--row-hover);
  }

  .ctx-row.danger {
    color: var(--err);
  }

  .ctx-row.danger:hover,
  .ctx-row.danger.active {
    background: color-mix(in srgb, var(--err) 12%, transparent);
  }

  .ctx-row:disabled {
    opacity: 0.45;
    cursor: default;
  }

  .ctx-sep {
    height: 1px;
    margin: 4px 6px;
    background: var(--edge);
  }
</style>
