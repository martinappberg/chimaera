<script lang="ts">
  import { basename } from "../files";
  import InlinePreview from "./InlinePreview.svelte";

  /**
   * The artifacts a turn produced, previewed after its closing prose — the
   * "here's what I made" moment. Images, CSV/TSV peeks, and PDFs render
   * themselves; a click opens the full native viewer in a pane. Backed by the
   * tools' absolute locations, so every tile opens regardless of how the
   * prose spelled the filename.
   */
  interface Props {
    paths: string[];
    onOpen?: (path: string) => void;
  }

  let { paths, onOpen }: Props = $props();
</script>

<div class="gallery" aria-label="artifacts produced this turn">
  {#each paths as path (path)}
    <figure class="tile">
      <InlinePreview {path} {onOpen} />
      <figcaption>
        <button class="cap" title="open {path} in a pane" onclick={() => onOpen?.(path)}>
          {basename(path)}
        </button>
      </figcaption>
    </figure>
  {/each}
</div>

<style>
  .gallery {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
    margin: 6px 0 10px;
  }
  .tile {
    margin: 0;
    flex: 1 1 260px;
    min-width: min(260px, 100%);
    max-width: 100%;
    border: 1px solid color-mix(in srgb, var(--edge) 70%, transparent);
    border-radius: 8px;
    overflow: hidden;
    background: color-mix(in srgb, var(--fg) 2%, transparent);
  }
  /* InlinePreview's own top border is redundant inside a bordered tile. */
  .tile :global(.artifact),
  .tile :global(.img-preview) {
    border-top: none;
  }
  figcaption {
    border-top: 1px solid color-mix(in srgb, var(--edge) 55%, transparent);
  }
  .cap {
    display: block;
    width: 100%;
    padding: 4px 10px;
    background: none;
    border: none;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-xs);
    font-family: var(--mono, monospace);
    text-align: left;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    cursor: pointer;
    transition: color 0.12s ease;
  }
  .cap:hover {
    color: var(--accent);
  }
</style>
