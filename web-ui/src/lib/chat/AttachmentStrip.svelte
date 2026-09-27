<script lang="ts" module>
  /** Picture sizes by saved path, so a strip that remounts (a queued bubble
   *  entering the transcript, a page of history coming back) reserves the
   *  right width before its resolve answers. Bounded; insertion-ordered. */
  const sizes = new Map<string, { width: number; height: number }>();
  const SIZES_MAX = 256;

  function rememberSize(path: string, width: number | undefined, height: number | undefined): void {
    if (width === undefined || height === undefined || sizes.has(path)) return;
    sizes.set(path, { width, height });
    if (sizes.size > SIZES_MAX) sizes.delete(sizes.keys().next().value as string);
  }
</script>

<script lang="ts">
  /**
   * A message's image attachments as a strip of picture tiles — the figure
   * strip of the agent's turn-end block, sized as a reference, not a
   * figure. One look for both sources, so what you attached in the
   * composer is what your message shows:
   *
   * - `drafts` (the composer): the pixels in memory, each tile with its
   *   remove button; a click previews the picture large.
   * - `paths` (a sent or queued message): the daemon's saved copies,
   *   resolved when the strip nears the viewport; a click opens the picture
   *   in a pane, as an agent's figure does. A copy that is gone says so in
   *   place (uploads end with their session).
   *
   * Tiles share one row height and take their width from the picture, so
   * nothing below moves when bytes arrive. No hover effects (the figure
   * strip's rule).
   */
  import FileIcon from "../shared/FileIcon.svelte";
  import { isMissing, rawUrl, resolveFile, type TargetResult } from "../shared/embed/embed";
  import { basename } from "../previews/files";
  import { attachmentSrc, tileBox, type ImageAttachment } from "./images";

  interface Props {
    /** Composer attachments (in memory). */
    drafts?: ImageAttachment[];
    onRemove?: (index: number) => void;
    onPreview?: (index: number) => void;
    /** A message's saved copies (absolute paths on the session's host). */
    paths?: string[];
    onOpen?: (path: string, e: MouseEvent) => void;
  }

  let { drafts, onRemove, onPreview, paths, onOpen }: Props = $props();

  /** Row height, and the width a tile may take from its picture. */
  const DRAFT = { row: 56, min: 40, max: 140 };
  const SAVED = { row: 112, min: 56, max: 240 };

  let host = $state<HTMLElement | null>(null);
  let near = $state(false);
  /** Resolve answers by path; absent = not asked yet, null = unreachable. */
  let answers = $state.raw<Record<string, TargetResult | null>>({});
  /** `/raw` URLs whose bytes failed to load — by URL, so a fresh ticket
   *  after a re-resolve gets its own try. */
  let broken = $state.raw<Set<string>>(new Set());
  /** Re-asks after an unreachable answer (a tunnel mid-reconnect): a few,
   *  spaced out, then the tile stays empty until it remounts. Bounded, so
   *  no visibility gate. */
  const RETRIES = 3;
  let attempt = $state(0);

  $effect(() => {
    const el = host;
    if (el === null || paths === undefined || near) return;
    if (typeof IntersectionObserver === "undefined") {
      near = true;
      return;
    }
    const observer = new IntersectionObserver(
      (entries) => {
        if (!entries.some((e) => e.isIntersecting)) return;
        near = true;
        observer.disconnect();
      },
      { root: el.closest(".transcript"), rootMargin: "480px 0px" },
    );
    observer.observe(el);
    return () => observer.disconnect();
  });

  // One coalesced resolve for the strip once it is near (resolveFile batches
  // every card asking in the same frame).
  $effect(() => {
    const wanted = paths;
    const tries = attempt;
    if (!near || wanted === undefined || wanted.length === 0) return;
    let stale = false;
    let retry: ReturnType<typeof setTimeout> | null = null;
    void Promise.all(wanted.map((p) => resolveFile(p))).then((results) => {
      if (stale) return;
      const next: Record<string, TargetResult | null> = {};
      wanted.forEach((p, i) => {
        const r = results[i];
        next[p] = r;
        if (r !== null && !isMissing(r)) rememberSize(p, r.width, r.height);
      });
      answers = next;
      if (results.includes(null) && tries < RETRIES) {
        retry = setTimeout(() => (attempt = tries + 1), 3000 * (tries + 1));
      }
    });
    return () => {
      stale = true;
      if (retry !== null) clearTimeout(retry);
    };
  });

  function markBroken(url: string): void {
    broken = new Set([...broken, url]);
  }

  function savedTitle(path: string, answer: TargetResult | null | undefined, failed: boolean): string {
    const name = basename(path);
    if (answer === null) return `${name} · couldn't reach the daemon`;
    if (answer !== undefined && isMissing(answer)) return `${name} · no longer on disk (uploads end with their session)`;
    if (failed) return `${name} · couldn't load this image`;
    return onOpen !== undefined ? `open ${name} in a pane` : name;
  }
</script>

{#if drafts !== undefined && drafts.length > 0}
  <div class="strip" role="group" aria-label="image attachments">
    {#each drafts as img, i (i)}
      {@const box = tileBox(img, DRAFT.row, DRAFT.min, DRAFT.max)}
      <div class="tile draft" style:width="{box.width}px" style:height="{box.height}px">
        <button
          class="pic"
          type="button"
          title="preview {img.label}"
          aria-label="preview {img.label}"
          onclick={() => onPreview?.(i)}
        >
          <img src={attachmentSrc(img)} alt={img.label} draggable="false" />
        </button>
        {#if onRemove !== undefined}
          <button
            class="remove"
            type="button"
            title="remove"
            aria-label="remove {img.label}"
            onclick={() => onRemove(i)}
          >
            <svg viewBox="0 0 10 10" width="8" height="8" aria-hidden="true"
              ><path d="M2 2l6 6M8 2 2 8" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" /></svg
            >
          </button>
        {/if}
      </div>
    {/each}
  </div>
{:else if paths !== undefined && paths.length > 0}
  <div class="strip saved" role="group" aria-label="image attachments" bind:this={host}>
    {#each paths as path (path)}
      {@const answer = answers[path]}
      {@const hit = answer !== undefined && answer !== null && !isMissing(answer) ? answer : null}
      {@const raw = hit !== null ? rawUrl(hit) : null}
      {@const failed = raw !== null && broken.has(raw)}
      {@const url = failed ? null : raw}
      {@const box = tileBox(hit ?? sizes.get(path) ?? null, SAVED.row, SAVED.min, SAVED.max)}
      {@const gone = (answer !== undefined && answer !== null && isMissing(answer)) || failed}
      <button
        class="tile pic saved-tile"
        class:gone
        type="button"
        style:width="{box.width}px"
        style:height="{box.height}px"
        title={savedTitle(path, answer, failed)}
        aria-label={savedTitle(path, answer, failed)}
        disabled={hit === null || onOpen === undefined}
        onclick={(e) => onOpen?.(path, e)}
      >
        {#if url !== null}
          <img
            src={url}
            alt={basename(path)}
            decoding="async"
            draggable="false"
            onerror={() => markBroken(url)}
          />
        {:else if gone}
          <FileIcon {path} size={18} broken />
        {/if}
      </button>
    {/each}
  </div>
{/if}

<style>
  .strip {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    gap: 6px;
  }
  /* A message's strip sits on the user's side, above the bubble. */
  .strip.saved {
    justify-content: flex-end;
  }
  .tile {
    position: relative;
    flex: none;
    max-width: 100%;
    overflow: hidden;
    border: 1px solid color-mix(in srgb, var(--edge) 75%, transparent);
    border-radius: 10px;
    background: color-mix(in srgb, var(--fg) 4%, transparent);
  }
  /* A tiny picture still gets a tile its ✕ doesn't cover. */
  .tile.draft {
    min-width: 32px;
    min-height: 32px;
    border-radius: 8px;
  }
  .pic {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 100%;
    height: 100%;
    padding: 0;
    border: none;
    border-radius: inherit;
    background: none;
    color: var(--muted);
    cursor: zoom-in;
  }
  /* A sent tile is itself the button: restore the tile's own edge, fill
     and corners over the button reset above. */
  .saved-tile {
    padding: 0;
    border: 1px solid color-mix(in srgb, var(--edge) 75%, transparent);
    border-radius: 10px;
    background: color-mix(in srgb, var(--fg) 4%, transparent);
    cursor: pointer;
  }
  .saved-tile:disabled {
    cursor: default;
  }
  /* Gone: the tile keeps its place, plainly empty. */
  .saved-tile.gone {
    border-style: dashed;
    background: none;
  }
  img {
    display: block;
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
  /* Always shown (a hover-only control is a hover effect, and touch has no
     hover): small and quiet over the corner, warming to --err on its own
     hover because it discards. */
  .remove {
    position: absolute;
    top: 3px;
    right: 3px;
    display: grid;
    place-items: center;
    width: 16px;
    height: 16px;
    padding: 0;
    border: 1px solid color-mix(in srgb, var(--edge) 80%, transparent);
    border-radius: 999px;
    background: color-mix(in srgb, var(--bg) 82%, transparent);
    color: var(--fg);
    cursor: pointer;
    transition:
      color 0.12s ease,
      border-color 0.12s ease;
  }
  .remove:hover {
    color: var(--err);
    border-color: color-mix(in srgb, var(--err) 55%, var(--edge));
  }
</style>
