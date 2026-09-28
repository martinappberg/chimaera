<script lang="ts">
  /**
   * One slide of a Marp deck (`#slide=N`, 1-based) drawn exactly as the
   * slides view draws it: Marp in the browser with raw HTML and scripts off,
   * math as MathML, the slide in a script-less sandboxed frame at its native
   * size, scaled to the card. The deck's relative images resolve in one
   * `resolve_targets` round trip to their `/raw` tickets.
   */
  import { dirname } from "../../previews/files";
  import { relativeImages, rewriteImages, slideDocument, slideSize } from "../../previews/marp";
  import { isMissing, resolveTargets } from "./embed";

  interface Props {
    path: string;
    source: string;
    slide: number;
    compact: boolean;
  }

  let { path, source, slide, compact }: Props = $props();

  const MAX_IMAGES = 200;

  let doc = $state<string | null>(null);
  let size = $state({ w: 1280, h: 720 });
  let count = $state(0);
  let shown = $state(1);
  let error = $state<string | null>(null);
  let boxW = $state(0);

  type MarpModule = typeof import("@marp-team/marp-core");

  async function build(p: string, src: string, n: number): Promise<void> {
    const targets = relativeImages(src).slice(0, MAX_IMAGES);
    const urls = new Map<string, string>();
    if (targets.length > 0) {
      try {
        const found = await resolveTargets(targets, dirname(p));
        for (const [t, r] of Object.entries(found)) {
          if (!isMissing(r) && r.ticket !== undefined) urls.set(t, `/raw/${r.ticket}`);
        }
      } catch {
        // Images draw broken, like any markdown view with a dead link.
      }
    }
    const mod: MarpModule = await import("@marp-team/marp-core");
    const marp = new mod.Marp({
      html: false,
      script: false,
      inlineSVG: true,
      math: { lib: "katex", katexOption: { output: "mathml", trust: false, maxExpand: 1000 }, katexFontPath: false },
      emoji: { shortcode: true, unicode: false },
    });
    const { html, css } = marp.render(rewriteImages(src, urls));
    // Parsed inert (DOMParser runs nothing, loads nothing): keep only the
    // wanted slide's SVG inside Marp's own wrapper, which its CSS targets.
    const parsed = new DOMParser().parseFromString(html, "text/html");
    const slides = parsed.querySelectorAll("svg[data-marpit-svg]");
    count = slides.length;
    const i = Math.min(Math.max(1, n), Math.max(1, slides.length));
    shown = i;
    const one = slides[i - 1]?.outerHTML ?? "";
    size = slideSize(html);
    doc = slideDocument(
      css.replace(/@font-face\s*\{[^}]*KaTeX[^}]*\}/g, ""),
      `<div class="marpit">${one}</div>`,
      size,
      "column",
    );
  }

  let gen = 0;
  $effect(() => {
    const p = path;
    const src = source;
    const n = slide;
    const mine = ++gen;
    error = null;
    build(p, src, n).catch((e: unknown) => {
      if (mine === gen) error = e instanceof Error ? e.message : "couldn't draw this slide";
    });
  });

  const scale = $derived(boxW > 0 ? boxW / size.w : 0);
</script>

<div class="slide-body" class:tile={compact}>
  {#if error !== null}
    <div class="note">{error}</div>
  {:else}
    <div class="stage" style:aspect-ratio="{size.w} / {size.h}" bind:clientWidth={boxW}>
      {#if doc !== null && scale > 0}
        <iframe
          title="slide {shown}"
          sandbox=""
          srcdoc={doc}
          style:width="{size.w}px"
          style:height="{size.h}px"
          style:transform="scale({scale})"
        ></iframe>
      {/if}
    </div>
    {#if count > 1}<span class="count">{shown} / {count}</span>{/if}
  {/if}
</div>

<style>
  .slide-body {
    position: relative;
    padding: 8px;
    background: color-mix(in srgb, var(--fg) 4%, transparent);
  }
  .slide-body.tile {
    flex: 1;
    display: flex;
    align-items: center;
    min-height: 0;
  }
  .stage {
    position: relative;
    width: 100%;
    max-width: 720px;
    margin: 0 auto;
    overflow: hidden;
    border-radius: 3px;
    box-shadow: 0 0 0 1px color-mix(in srgb, var(--edge) 80%, transparent);
  }
  .tile .stage {
    max-width: calc(184px * 16 / 9);
  }
  iframe {
    position: absolute;
    top: 0;
    left: 0;
    border: none;
    transform-origin: 0 0;
    pointer-events: none;
  }
  .count {
    position: absolute;
    right: 14px;
    bottom: 14px;
    padding: 1px 6px;
    border-radius: 999px;
    background: color-mix(in srgb, var(--bg) 85%, transparent);
    color: var(--muted);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }
  .note {
    padding: 6px 2px;
    color: var(--muted);
    font-size: var(--text-xs);
  }
</style>
