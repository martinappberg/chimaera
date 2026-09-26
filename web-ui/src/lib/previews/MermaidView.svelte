<script lang="ts">
  /**
   * A `.mmd` / `.mermaid` file drawn as its diagram, through the shared,
   * sandboxed renderer (`shared/mermaid.ts`: strict security level, sanitized
   * SVG). It re-renders when the theme flips or the file changes on disk (an
   * agent editing the diagram), keeping the last good drawing on screen with
   * the parse error beside it rather than blanking. Fit-to-pane by default;
   * zoom steps and 1:1 when a large diagram needs reading. Exports the SVG as
   * drawn, or a PNG at twice its size.
   */
  import type { Snippet } from "svelte";
  import { basename, type FileChunk } from "./files";
  import { MERMAID_MAX_SOURCE, MermaidError, renderMermaid } from "../shared/mermaid";
  import { activeTheme } from "../settings/store.svelte";
  import Spinner from "./Spinner.svelte";

  interface Props {
    path: string;
    /** The file's current bytes (FileView's store chunk; refreshed in place
     *  on a disk change). */
    chunk: FileChunk;
    /** The host's view switch (diagram ⇄ source), drawn at the bar's end. */
    switcher?: Snippet;
  }

  let { path, chunk, switcher }: Props = $props();

  const theme = $derived(activeTheme().kind);
  const source = $derived(new TextDecoder().decode(chunk.bytes));
  const tooBig = $derived(chunk.truncated || source.length > MERMAID_MAX_SOURCE);

  let svg = $state<string | null>(null);
  let error = $state<string | null>(null);
  let rendering = $state(false);

  $effect(() => {
    const src = source;
    const t = theme;
    if (tooBig) return;
    let stale = false;
    rendering = true;
    renderMermaid(src, t).then(
      (out) => {
        if (stale) return;
        svg = out;
        error = null;
        rendering = false;
      },
      (e: unknown) => {
        if (stale) return;
        error = e instanceof MermaidError ? e.message : "the diagram could not be drawn";
        rendering = false;
      },
    );
    return () => {
      stale = true;
    };
  });

  /** null = fit to the pane; otherwise a factor of the diagram's own size. */
  let zoom = $state<number | null>(null);
  const ZOOMS = [0.25, 0.5, 0.75, 1, 1.5, 2, 3, 4];
  let stageEl = $state<HTMLDivElement | null>(null);
  // The diagram's intrinsic size, from its viewBox (mermaid sets max-width
  // inline and width="100%"; the drawing is sized from here instead).
  const natural = $derived.by(() => {
    const m = svg === null ? null : /viewBox="\s*[-\d.]+\s+[-\d.]+\s+([\d.]+)\s+([\d.]+)"/.exec(svg);
    return m === null ? null : { w: Number(m[1]), h: Number(m[2]) };
  });

  function step(dir: 1 | -1): void {
    const el = stageEl;
    const n = natural;
    const cur =
      zoom ??
      (el !== null && n !== null
        ? Math.min((el.clientWidth - 40) / n.w, (el.clientHeight - 40) / n.h)
        : 1);
    const next =
      dir > 0 ? ZOOMS.find((z) => z > cur + 0.01) : [...ZOOMS].reverse().find((z) => z < cur - 0.01);
    zoom = next ?? (dir > 0 ? ZOOMS[ZOOMS.length - 1] : ZOOMS[0]);
  }

  const sized = $derived(
    zoom !== null && natural !== null ? { w: natural.w * zoom, h: natural.h * zoom } : null,
  );

  function stem(): string {
    return basename(path).replace(/\.(mmd|mermaid)$/i, "") || "diagram";
  }

  /** The drawing as a standalone SVG document. */
  function svgDocument(): string | null {
    const s = svg;
    if (s === null) return null;
    const withNs = s.includes('xmlns="http://www.w3.org/2000/svg"')
      ? s
      : s.replace("<svg", '<svg xmlns="http://www.w3.org/2000/svg"');
    return `<?xml version="1.0" encoding="UTF-8"?>\n${withNs}`;
  }

  function save(blob: Blob, name: string): void {
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = name;
    a.rel = "noopener";
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 10_000);
  }

  let exportNote = $state<string | null>(null);
  let noteTimer: ReturnType<typeof setTimeout> | null = null;
  function note(text: string): void {
    exportNote = text;
    if (noteTimer !== null) clearTimeout(noteTimer);
    noteTimer = setTimeout(() => (exportNote = null), 3000);
  }
  $effect(() => () => {
    if (noteTimer !== null) clearTimeout(noteTimer);
  });

  function exportSvg(): void {
    const doc = svgDocument();
    if (doc === null) return;
    save(new Blob([doc], { type: "image/svg+xml" }), `${stem()}.svg`);
  }

  /** Rasterize through an <img> (the SVG never runs anything there) onto a
   *  canvas at 2x, on the theme's pane color so it reads as it does here. */
  async function exportPng(): Promise<void> {
    const doc = svgDocument();
    const n = natural;
    if (doc === null || n === null) return;
    const img = new Image();
    img.decoding = "async";
    img.src = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(doc)}`;
    try {
      await img.decode();
      const k = 2;
      const canvas = document.createElement("canvas");
      canvas.width = Math.ceil(n.w * k);
      canvas.height = Math.ceil(n.h * k);
      const ctx = canvas.getContext("2d");
      if (ctx === null) throw new Error("no canvas");
      ctx.fillStyle = getComputedStyle(stageEl ?? document.body).getPropertyValue("--term-bg").trim() || "white";
      ctx.fillRect(0, 0, canvas.width, canvas.height);
      ctx.drawImage(img, 0, 0, canvas.width, canvas.height);
      const blob = await new Promise<Blob | null>((resolve) => canvas.toBlob(resolve, "image/png"));
      if (blob === null) throw new Error("no image");
      save(blob, `${stem()}.png`);
    } catch {
      // A diagram with HTML labels taints the canvas in some engines.
      note("PNG export isn't available for this diagram — export SVG instead");
    }
  }
</script>

<div class="mermaid-view">
  <div class="mm-bar">
    {#if svg !== null}
      <button class="bbtn icon" onclick={() => step(-1)} title="zoom out" aria-label="zoom out">
        <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true"><path d="M3.5 8h9" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" /></svg>
      </button>
      <button class="bbtn zoom" class:on={zoom === null} onclick={() => (zoom = null)} title="fit to the pane">
        {zoom === null ? "fit" : `${Math.round(zoom * 100)}%`}
      </button>
      <button class="bbtn icon" onclick={() => step(1)} title="zoom in" aria-label="zoom in">
        <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true"><path d="M3.5 8h9M8 3.5v9" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" /></svg>
      </button>
      <button class="bbtn" class:on={zoom === 1} onclick={() => (zoom = 1)} title="actual size">1:1</button>
    {/if}
    {#if exportNote !== null}
      <span class="bar-note" role="status">{exportNote}</span>
    {:else if error !== null && svg !== null}
      <span class="bar-err" title={error}>{error}</span>
    {/if}
    <span class="spacer"></span>
    {#if svg !== null}
      <button class="bbtn" onclick={exportSvg} title="download the diagram as SVG">svg</button>
      <button class="bbtn" onclick={() => void exportPng()} title="download the diagram as PNG">png</button>
    {/if}
    {@render switcher?.()}
  </div>

  <div class="stage" class:fit={sized === null} bind:this={stageEl}>
    {#if tooBig}
      <div class="mm-msg">
        <span>this diagram is too large to draw here</span>
        <span class="hint">{MERMAID_MAX_SOURCE.toLocaleString()} characters at most — the source view shows it</span>
      </div>
    {:else if svg !== null}
      <!-- Sanitized by shared/mermaid (DOMPurify, svg profile). -->
      <!-- Fit shrinks a large diagram to the pane but never blows a small
           one up past its own size. -->
      <div
        class="drawing"
        class:dim={rendering}
        style:width={sized === null ? null : `${sized.w}px`}
        style:height={sized === null ? null : `${sized.h}px`}
        style:max-width={sized === null && natural !== null ? `${natural.w}px` : null}
        style:max-height={sized === null && natural !== null ? `${natural.h}px` : null}
      >
        {@html svg}
      </div>
    {:else if error !== null}
      <div class="mm-msg">
        <span class="err-title">the diagram has an error</span>
        <pre class="err-text">{error}</pre>
      </div>
    {:else}
      <Spinner label="drawing" />
    {/if}
  </div>
</div>

<style>
  .mermaid-view {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
  }

  .mm-bar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.3rem;
    height: 26px;
    padding: 0 0.5rem 0 0.4rem;
    border-bottom: 1px solid var(--edge);
    font-size: var(--text-xs);
    color: var(--muted);
    min-width: 0;
  }

  .spacer {
    flex: 1;
  }

  .bar-err,
  .bar-note {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    margin-left: 0.4rem;
  }

  .bar-err {
    color: var(--err);
  }

  .bbtn {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: pointer;
    padding: 0.1rem 0.45rem;
    border-radius: 4px;
    white-space: nowrap;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .bbtn.icon {
    display: inline-flex;
    align-items: center;
    padding: 0.2rem 0.3rem;
  }

  .bbtn.zoom {
    min-width: 4.5ch;
    font-family: var(--mono);
    font-variant-numeric: tabular-nums;
  }

  .bbtn:hover {
    background: var(--row-hover);
    color: var(--fg);
  }

  .bbtn.on {
    color: var(--fg);
  }

  .stage {
    position: relative;
    flex: 1;
    min-height: 0;
    overflow: auto;
    scrollbar-width: thin;
    display: grid;
    padding: 20px;
  }

  .drawing {
    margin: auto;
    transition: opacity 0.15s ease;
  }

  .drawing.dim {
    opacity: 0.6;
  }

  /* Fit: the drawing takes the pane's box and the SVG scales inside it. */
  .stage.fit .drawing {
    width: 100%;
    height: 100%;
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .drawing :global(svg) {
    display: block;
    width: 100%;
    height: 100%;
    max-width: none !important;
  }

  .stage.fit .drawing :global(svg) {
    max-width: 100% !important;
    max-height: 100%;
  }

  .mm-msg {
    margin: auto;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.5rem;
    max-width: min(640px, 100%);
    color: var(--muted);
    font-size: var(--text-md);
    text-align: center;
  }

  .hint {
    font-size: var(--text-xs);
    opacity: 0.8;
  }

  .err-title {
    color: var(--err);
  }

  .err-text {
    margin: 0;
    max-width: 100%;
    overflow: auto;
    padding: 0.6rem 0.8rem;
    border: 1px solid color-mix(in srgb, var(--err) 35%, var(--edge));
    border-radius: 6px;
    background: color-mix(in srgb, var(--err) 6%, transparent);
    color: var(--fg);
    font-family: var(--mono);
    font-size: var(--text-xs);
    text-align: left;
    white-space: pre-wrap;
  }

  @media (prefers-reduced-motion: reduce) {
    .bbtn,
    .drawing {
      transition: none;
    }
  }
</style>
