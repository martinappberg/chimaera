<script lang="ts">
  /**
   * The hover preview's popover (driven by `hoverController.svelte.ts`), on the
   * app's floating surface: a document's section drawn by the reading
   * renderer under a thin header, or an embed card's compact body, placed
   * beside the link inside the view's content box. Its content is inert — links don't follow, nothing
   * takes focus — but it scrolls; the frame itself only reports the pointer
   * coming and going, so moving onto it keeps it open. Outside a reading
   * view (chat) it is `standalone` and brings the document typography.
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
  class:standalone={s.standalone}
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
      {#if s.note !== ""}<span class="hp-flag">{s.note}</span>{/if}
    </div>
  {:else if s.note !== ""}
    <p class="hp-flag">{s.note}</p>
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

  /* A glance's height: a picture, a PDF page, a table's first rows. */
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

  /* What the host knows about the file ("changed after this turn"): at
     the header's end, or above an embed card (which has its own head). */
  .hp-flag {
    flex: none;
    margin: 0 0 0 auto;
    padding: 0 7px;
    border-radius: 999px;
    background: color-mix(in srgb, var(--accent) 10%, transparent);
    color: color-mix(in srgb, var(--accent) 80%, var(--fg));
    font-size: var(--text-xs);
    white-space: nowrap;
  }

  .card .hp-flag {
    align-self: flex-start;
    margin: 0 0 4px;
    border: 1px solid var(--edge);
    background: var(--overlay-bg);
    line-height: 1.6;
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

  /* Outside a reading view nothing styles the document's blocks, so the
     popover carries the reading view's own rules (MarkdownView's
     `.md-view .md-doc`, which are scoped to it) for what a section draws.
     Tables are app.css's; equations the global .katex rule. */
  .standalone .hp-content :global(.md-doc :is(h1, h2, h3, h4, h5, h6)) {
    line-height: 1.25;
    margin: 1.6em 0 0.55em;
    font-weight: 600;
    letter-spacing: -0.01em;
  }

  .standalone .hp-content :global(.md-doc h1) {
    font-size: 1.576em;
    margin-top: 0.2em;
    padding-bottom: 0.35em;
    border-bottom: 1px solid var(--edge);
  }

  .standalone .hp-content :global(.md-doc h2) {
    font-size: 1.25em;
    padding-bottom: 0.25em;
    border-bottom: 1px solid var(--edge);
  }

  .standalone .hp-content :global(.md-doc h3) {
    font-size: 1.087em;
  }

  .standalone .hp-content :global(.md-doc :is(h4, h5, h6)) {
    font-size: 1em;
  }

  .standalone .hp-content :global(.md-doc p) {
    margin: 0.7em 0;
  }

  .standalone .hp-content :global(.md-doc a) {
    color: var(--accent);
    text-decoration: none;
  }

  .standalone .hp-content :global(.md-doc code) {
    font-family: var(--mono);
    font-size: 0.82em;
    background: color-mix(in srgb, var(--fg) 6%, transparent);
    border-radius: 4px;
    padding: 0.12em 0.34em;
  }

  .standalone .hp-content :global(.md-doc pre) {
    background: color-mix(in srgb, var(--fg) 4.5%, transparent);
    border: 1px solid var(--edge);
    border-radius: 8px;
    padding: 0.8em 1em;
    overflow: hidden;
    line-height: 1.5;
  }

  .standalone .hp-content :global(.md-doc pre code) {
    display: block;
    overflow-x: auto;
    scrollbar-width: thin;
    background: none;
    padding: 0;
    font-size: 0.848em;
  }

  .standalone .hp-content :global(.md-doc blockquote) {
    margin: 0.8em 0;
    padding: 0.55em 1em;
    border-left: 3px solid color-mix(in srgb, var(--accent) 60%, transparent);
    border-radius: 0 8px 8px 0;
    background: linear-gradient(
      to right,
      color-mix(in srgb, var(--accent) 5%, transparent),
      color-mix(in srgb, var(--fg) 3%, transparent) 55%
    );
    color: color-mix(in srgb, var(--fg) 45%, var(--muted));
  }

  .standalone .hp-content :global(.md-doc blockquote > :first-child) {
    margin-top: 0;
  }

  .standalone .hp-content :global(.md-doc blockquote > :last-child) {
    margin-bottom: 0;
  }

  .standalone .hp-content :global(.md-doc :is(ul, ol)) {
    padding-left: 1.6em;
    margin: 0.6em 0;
  }

  .standalone .hp-content :global(.md-doc li) {
    margin: 0.2em 0;
  }

  .standalone .hp-content :global(.md-doc li::marker) {
    color: color-mix(in srgb, var(--accent) 70%, var(--muted));
  }

  /* Task boxes, as the reading view draws them. */
  .standalone .hp-content :global(.md-doc .md-task) {
    display: inline-block;
    position: relative;
    width: 0.92em;
    height: 0.92em;
    margin: 0 0.45em 0 0;
    vertical-align: -0.12em;
    box-sizing: border-box;
    border: 1.5px solid color-mix(in srgb, var(--fg) 38%, transparent);
    border-radius: 3px;
    background: var(--term-bg);
  }

  .standalone .hp-content :global(.md-doc .md-task[data-task="done"]) {
    background: var(--accent);
    border-color: var(--accent);
  }

  .standalone .hp-content :global(.md-doc .md-task[data-task="done"]::after) {
    content: "";
    position: absolute;
    left: 30%;
    top: 8%;
    width: 28%;
    height: 58%;
    border: solid var(--term-bg);
    border-width: 0 0.13em 0.13em 0;
    transform: rotate(45deg);
  }

  .standalone .hp-content :global(.md-doc ul > li.md-task-item) {
    list-style: none;
  }

  .standalone .hp-content :global(.md-doc ul > li.md-task-item > .md-task:first-child),
  .standalone .hp-content :global(.md-doc ul > li.md-task-item > p:first-child > .md-task:first-child) {
    margin-left: -1.35em;
    margin-right: 0.43em;
  }

  .standalone .hp-content :global(.md-doc .md-task-text) {
    color: var(--muted);
    text-decoration: line-through;
    text-decoration-color: color-mix(in srgb, var(--muted) 70%, transparent);
  }

  .standalone .hp-content :global(.md-doc hr) {
    border: none;
    border-top: 1px solid var(--edge);
    margin: 1em 0;
  }

  .standalone .hp-content :global(.md-doc img) {
    max-width: 100%;
  }

  .standalone .hp-content :global(.md-doc .md-math-display) {
    display: block;
    max-width: 100%;
    overflow-x: auto;
    overflow-y: hidden;
    margin: 0.55em 0;
  }

  .standalone .hp-content :global(.md-doc .markdown-alert) {
    margin: 0.8em 0;
    padding: 0.45em 0.9em;
    border-left: 3px solid color-mix(in srgb, var(--accent) 60%, transparent);
  }

  .standalone .hp-content :global(.md-doc .markdown-alert-title) {
    font-weight: 600;
  }

  @media (prefers-reduced-motion: reduce) {
    .hover-preview {
      transition: none;
      transform: none;
    }
  }
</style>
