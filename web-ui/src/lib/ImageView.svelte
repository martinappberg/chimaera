<script lang="ts">
  import { fsRawUrl } from "./files";

  interface Props {
    path: string;
  }

  let { path }: Props = $props();

  let url = $state<string | null>(null);
  let error = $state<string | null>(null);
  /** Zoom-to-fit by default; click toggles actual size (1:1 pixels). */
  let fit = $state(true);
  let natural = $state<{ w: number; h: number } | null>(null);

  $effect(() => {
    const p = path;
    url = null;
    error = null;
    fit = true;
    natural = null;
    let stale = false;
    fsRawUrl(p)
      .then((u) => {
        if (!stale) url = u;
      })
      .catch((e) => {
        if (!stale) error = e instanceof Error ? e.message : "failed to load image";
      });
    return () => {
      stale = true;
    };
  });

  function onLoad(e: Event): void {
    const img = e.currentTarget as HTMLImageElement;
    natural = { w: img.naturalWidth, h: img.naturalHeight };
  }
</script>

<div class="image-view" class:fit>
  {#if error !== null}
    <div class="file-error">{error}</div>
  {:else if url !== null}
    <button
      class="frame"
      class:fit
      title={fit ? "click for actual size" : "click to fit"}
      onclick={() => (fit = !fit)}
    >
      <img
        src={url}
        alt={path}
        class:fit
        onload={onLoad}
        onerror={() => (error = "image failed to load (ticket may have expired — reopen the tab)")}
        draggable="false"
      />
    </button>
    {#if natural !== null}
      <span class="dims">{natural.w}×{natural.h}{fit ? "" : " · 1:1"}</span>
    {/if}
  {/if}
</div>

<style>
  .image-view {
    position: absolute;
    inset: 0;
    overflow: auto;
    display: flex;
  }

  .frame {
    appearance: none;
    border: none;
    background: none;
    padding: 18px;
    margin: auto; /* centers when smaller than the pane, scrolls when larger */
    cursor: zoom-out;
    line-height: 0;
  }

  .frame.fit {
    cursor: zoom-in;
    width: 100%;
    height: 100%;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 18px;
  }

  img {
    display: block;
    image-rendering: auto;
  }

  img.fit {
    max-width: 100%;
    max-height: 100%;
    object-fit: contain;
  }

  .dims {
    position: absolute;
    right: 10px;
    bottom: 8px;
    font-family: var(--mono);
    font-size: 0.62rem;
    color: var(--muted);
    background: color-mix(in srgb, var(--term-bg) 78%, transparent);
    padding: 1px 6px;
    border-radius: 4px;
    pointer-events: none;
  }

  .file-error {
    margin: auto;
    color: var(--muted);
    font-size: 0.8rem;
    padding: 1rem;
    text-align: center;
  }
</style>
