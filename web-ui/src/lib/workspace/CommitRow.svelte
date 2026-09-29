<script lang="ts">
  /**
   * One compact history row: subject (truncated), author muted, relative time
   * right-aligned and muted. Hover shows the full message and a short sha;
   * click opens the commit. The leading slot is reserved for a small agent
   * glyph (a commit a session made) that the session record adds later.
   */
  import type { GitCommit } from "./git";
  import { commitHover, relTime } from "./gitFormat";

  interface Props {
    commit: GitCommit;
    onOpen: (e: MouseEvent) => void;
    /** Start a drag (a commit drops into a chat composer like a file). */
    onDragStart?: (e: PointerEvent) => void;
    active?: boolean;
  }
  let { commit, onOpen, onDragStart, active = false }: Props = $props();
</script>

<button
  class="crow"
  class:active
  title={commitHover(commit)}
  onclick={(e) => {
    // A pointer press is the drag controller's (a release without a drag
    // opens through it); the click only opens from the keyboard.
    if (!onDragStart || e.detail === 0) onOpen(e);
  }}
  onpointerdown={(e) => {
    if (e.button === 0 && onDragStart) onDragStart(e);
  }}
>
  <span class="cslot" aria-hidden="true"></span>
  <span class="csubject">{commit.subject || "(no message)"}</span>
  <span class="cauthor">{commit.author}</span>
  <span class="ctime">{relTime(commit.time)}</span>
</button>

<style>
  .crow {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    width: 100%;
    min-height: calc(var(--text-sm) + 9px);
    padding: 0 0.7rem 0 0.45rem;
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    text-align: left;
    cursor: pointer;
    color: var(--fg);
  }
  .crow:hover,
  .crow.active {
    background: var(--row-hover);
  }
  .crow:focus-visible {
    outline: 1px solid var(--focus-ring);
    outline-offset: -1px;
  }
  .cslot {
    flex: none;
    width: 12px;
  }
  .csubject {
    flex: 1 1 auto;
    min-width: 0;
    font-size: var(--text-sm);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .cauthor {
    flex: 0 1 auto;
    max-width: 9em;
    font-size: var(--text-xs);
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .ctime {
    flex: none;
    min-width: 2.2em;
    text-align: right;
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    color: var(--muted);
  }
</style>
