<script lang="ts">
  /**
   * Video or audio in a card: the native player over ranged `/raw` reads
   * (seeking fetches only what it plays), starting at a `#t=start,end`
   * moment — media fragments the browser honors natively. Only metadata is
   * fetched until the reader presses play. A format this browser cannot
   * decode says so, with a download on a remote host.
   */
  import { mediaUrl } from "./embed";
  import type { Locator } from "../reveal";

  interface Props {
    url: string | null;
    kind: "video" | "audio";
    time: Locator["time"];
    compact: boolean;
    active: boolean;
    onDownload?: () => void;
  }

  let { url, kind, time, compact, active, onDownload }: Props = $props();

  let aspect = $state("16 / 9");
  let failed = $state(false);
  $effect(() => {
    void url;
    failed = false;
  });

  const src = $derived(url !== null ? mediaUrl(url, time) : null);

  function onMeta(e: Event): void {
    const v = e.currentTarget as HTMLVideoElement;
    if (v.videoWidth > 0 && v.videoHeight > 0) aspect = `${v.videoWidth} / ${v.videoHeight}`;
  }
</script>

<div class="media-body" class:tile={compact} class:audio={kind === "audio"}>
  {#if failed}
    <div class="note">
      <span>this browser can't play this {kind === "video" ? "video" : "audio"} file</span>
      {#if onDownload !== undefined}<button onclick={onDownload}>download</button>{/if}
    </div>
  {:else if kind === "video"}
    <div class="frame" style:aspect-ratio={aspect}>
      {#if active && src !== null}
        <!-- svelte-ignore a11y_media_has_caption -->
        <video {src} controls preload="metadata" playsinline onloadedmetadata={onMeta} onerror={() => (failed = true)}></video>
      {/if}
    </div>
  {:else if active && src !== null}
    <audio {src} controls preload="metadata" onerror={() => (failed = true)}></audio>
  {:else}
    <div class="audio-slot"></div>
  {/if}
</div>

<style>
  .media-body {
    padding: 8px;
    background: color-mix(in srgb, var(--fg) 4%, transparent);
  }
  .media-body.tile {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    min-height: 0;
  }
  .frame {
    position: relative;
    width: 100%;
    max-width: 720px;
    max-height: 420px;
    margin: 0 auto;
    border-radius: 4px;
    overflow: hidden;
    background: #000;
  }
  .tile .frame {
    max-height: 184px;
  }
  video {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: contain;
  }
  audio,
  .audio-slot {
    display: block;
    width: 100%;
    height: 40px;
  }
  .note {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 12px;
    color: var(--muted);
    font-size: var(--text-xs);
  }
  .note button {
    padding: 2px 8px;
    border: 1px solid var(--edge);
    border-radius: 4px;
    background: none;
    color: var(--fg);
    font: inherit;
    cursor: pointer;
  }
  .note button:hover {
    color: var(--accent);
    border-color: var(--accent);
  }
</style>
