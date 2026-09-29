<script module lang="ts">
  export type ChipState = "present" | "changed" | "gone" | "missing" | "pending";
</script>

<script lang="ts">
  /**
   * A file named on a chip: every file on the "written this turn" line (a
   * figure as much as a note) and a document the prose embeds
   * (`![plan](analysis/PLAN.md)`). Click opens it;
   * resting the pointer on it previews it — the chat's hover controller
   * finds the chip through the registry (`hoverTargets.ts`), so only a chip
   * whose file is known registers. A gone or not-found file keeps its name,
   * plainly not there.
   */
  import FileIcon from "../shared/FileIcon.svelte";
  import type { ChatHoverTarget, HoverTargets } from "./hoverTargets";

  interface Props {
    /** The file: absolute once known, else as written. */
    path: string;
    label: string;
    state?: ChipState;
    /** Opens the file; absent, the chip only names it. */
    onOpen?: (e: MouseEvent) => void;
    /** Where the chip registers its preview, and what it previews. */
    hover?: { targets: HoverTargets; target: ChatHoverTarget } | null;
    /** The accessible name (defaults to "open <label>"). */
    name?: string;
    /** A tooltip, for what no preview shows (gone, not found). */
    title?: string;
    onEnter?: () => void;
  }

  let { path, label, state = "present", onOpen, hover = null, name, title, onEnter }: Props = $props();

  const absent = $derived(state === "gone" || state === "missing");

  function register(el: HTMLElement) {
    const h = hover;
    if (h === null) return;
    h.targets.set(el, h.target);
    return () => h.targets.delete(el);
  }
</script>

<button
  class="chip"
  class:absent
  class:gone={state === "gone"}
  aria-label={name ?? (onOpen !== undefined ? `open ${label}` : label)}
  {title}
  disabled={onOpen === undefined || state === "gone"}
  onclick={(e) => onOpen?.(e)}
  onpointerenter={onEnter}
  {@attach register}
>
  <FileIcon {path} size={13} broken={absent} />
  <span class="name">{label}</span>
</button>

<style>
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    max-width: 100%;
    padding: 1px 7px 1px 5px;
    border: 1px solid var(--edge);
    border-radius: 999px;
    background: color-mix(in srgb, var(--fg) 3%, transparent);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-xs);
    line-height: 1.5;
    cursor: pointer;
    transition:
      border-color 0.12s ease,
      background-color 0.12s ease,
      color 0.12s ease;
  }
  .chip:hover:not(:disabled),
  .chip:focus-visible {
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
    background: color-mix(in srgb, var(--accent) 9%, transparent);
  }
  .chip:disabled {
    cursor: default;
  }
  .name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono, monospace);
  }
  /* Gone or not found: still named, plainly not there. */
  .chip.absent {
    color: var(--muted);
    border-style: dashed;
    background: none;
  }
  /* Gone: the turn did write it, and it has since been deleted. */
  .chip.gone .name {
    text-decoration: line-through;
  }
</style>
