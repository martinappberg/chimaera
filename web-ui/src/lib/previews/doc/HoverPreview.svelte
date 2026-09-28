<script lang="ts">
  /**
   * The hover preview's popover (driven by `hoverController.svelte.ts`), on the
   * app's floating surface: a document's section drawn by the reading
   * renderer under a thin header, or an embed card's compact body, placed
   * beside the link inside the view's content box. Its content is inert — links don't follow, nothing
   * takes focus — but it scrolls; the frame itself only reports the pointer
   * coming and going, so moving onto it keeps it open.
   */
  import FileIcon from "../../shared/FileIcon.svelte";
  import type { PreviewState } from "./hoverController.svelte";

  let { s }: { s: PreviewState } = $props();

  /** The controller's content node, adopted into the scroller. */
  function adopt(node: HTMLElement) {
    return (el: HTMLElement) => {
      el.append(node);
      return () => node.remove();
    };
  }
</script>

<div
  class="hover-preview overlay-surface"
  class:card={s.kind === "card"}
  class:shown={s.visible}
  id={s.id}
  role="tooltip"
  style:left="{s.place.left}px"
  style:top={s.place.top === null ? null : `${s.place.top}px`}
  style:bottom={s.place.bottom === null ? null : `${s.place.bottom}px`}
  style:width="{s.place.width}px"
  style:max-height="{s.place.maxHeight}px"
  style:--hp-font="{s.fontSize}px"
  onpointerenter={s.onEnter}
  onpointerleave={s.onLeave}
>
  {#if s.kind === "doc"}
    <div class="hp-head">
      <FileIcon path={s.path} size={12} />
      <span class="hp-name">{s.name}</span>
      {#if s.label !== ""}<span class="hp-frag">{s.label}</span>{/if}
    </div>
  {/if}
  <div class="hp-scroll">
    <div class="hp-content" inert {@attach adopt(s.content)}></div>
    {#if s.message !== ""}
      <p class="hp-note" class:quiet={s.status === "loading"}>{s.message}</p>
    {/if}
  </div>
</div>

<style>
  /* The app's floating surface (app.css .overlay-surface: the menus'
     background, edge, radius and shadow), edge to edge. */
  .hover-preview {
    z-index: 6;
    display: flex;
    flex-direction: column;
    min-height: 0;
    padding: 0;
    color: var(--fg);
    opacity: 0;
    transform: translateY(2px);
    transition:
      opacity 0.12s ease,
      transform 0.12s ease;
  }

  .hover-preview.shown {
    opacity: 1;
    transform: none;
  }

  /* An embed card is its own frame. */
  .hover-preview.card {
    border-color: transparent;
    background: none;
  }

  .hover-preview.card :global(.embed-card) {
    margin: 0;
    background: var(--overlay-bg);
  }

  /* A glance, not a card to act on: the link itself opens the file. */
  .hover-preview.card :global(.embed-card .head .act) {
    display: none;
  }

  .hp-head {
    flex: none;
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    padding: 5px 10px 4px;
    border-bottom: 1px solid color-mix(in srgb, var(--edge) 70%, transparent);
    font-size: var(--text-xs);
    color: var(--muted);
  }

  .hp-name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono);
    color: var(--fg);
  }

  .hp-frag {
    flex: none;
    max-width: 50%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    padding: 0 6px;
    border-radius: 999px;
    background: color-mix(in srgb, var(--accent) 10%, transparent);
    color: color-mix(in srgb, var(--accent) 80%, var(--fg));
  }

  .hp-scroll {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    overscroll-behavior: contain;
    scrollbar-width: thin;
  }

  .card .hp-scroll {
    overflow: hidden;
  }

  /* The gallery tile's height (chat's ArtifactGallery). */
  .card .hp-content :global(.hp-card) {
    height: 252px;
  }

  /* The document's section: the reading view's own `.md-doc` rules (this
     sits inside the view), a step smaller. */
  .hp-content :global(.md-doc) {
    padding: 0.5rem 0.9rem 0.7rem;
    font-size: calc(var(--hp-font) * 0.92);
    line-height: var(--markdown-line-height, 1.6);
    overflow-wrap: break-word;
  }

  .hp-content :global(.md-doc > :first-child) {
    margin-top: 0.2em;
  }

  .hp-content :global(.md-doc > :last-child) {
    margin-bottom: 0;
  }

  .hp-note {
    margin: 0;
    padding: 0.55rem 0.9rem;
    font-size: var(--text-xs);
    color: var(--muted);
  }

  .hp-note.quiet {
    opacity: 0.8;
  }

  @media (prefers-reduced-motion: reduce) {
    .hover-preview {
      transition: none;
      transform: none;
    }
  }
</style>
