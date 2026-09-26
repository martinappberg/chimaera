<script lang="ts">
  /**
   * One `text/html` notebook output (a DataFrame, a styled report) in an
   * iframe sandboxed with no permissions at all: no script, no same-origin,
   * no forms, no popups. The frame cannot tell us its height, so the height
   * comes from laying out a sanitized, media-free copy offscreen — close for
   * the tables and text these outputs usually are — capped with an expand
   * control, and the box is drag-resizable for anything it misjudges.
   */
  import DOMPurify from "dompurify";
  import { activeTheme } from "../settings/store.svelte";

  interface Props {
    html: string;
  }

  let { html }: Props = $props();

  /** Tallest a collapsed output is drawn (px). */
  const MAX_H = 440;
  const MIN_H = 36;
  /** Ceiling when expanded, so a runaway table still ends. */
  const EXPANDED_MAX = 6000;
  const FONT = "13px/1.45 system-ui, -apple-system, 'Segoe UI', sans-serif";

  const purify = DOMPurify(window);

  let width = $state(0);
  let natural = $state<number | null>(null);
  let expanded = $state(false);

  // The frame's colors: the live theme's token values, written as literals
  // (a sandboxed document cannot read the parent's custom properties).
  const doc = $derived.by(() => {
    const theme = activeTheme();
    const cs = getComputedStyle(document.documentElement);
    const v = (name: string, fallback: string) => cs.getPropertyValue(name).trim() || fallback;
    const fg = v("--fg", theme.tokens["--fg"]);
    const muted = v("--muted", theme.tokens["--muted"]);
    const edge = v("--edge", theme.tokens["--edge"]);
    const zebra = v("--row-hover", theme.tokens["--row-hover"]);
    const accent = v("--accent", theme.tokens["--accent"]);
    const css = `:root{color-scheme:${theme.kind}}
html,body{margin:0;background:transparent}
body{padding:4px 2px;font:${FONT};color:${fg};overflow-wrap:break-word}
a{color:${accent}}
table{border-collapse:collapse;font-size:12px;font-variant-numeric:tabular-nums}
th,td{border:1px solid ${edge};padding:3px 9px;text-align:right;vertical-align:top}
thead th{border-bottom-width:2px}
tbody tr:nth-child(even){background:${zebra}}
caption,small{color:${muted}}
img{max-width:100%}
pre,code{font-family:ui-monospace,Menlo,monospace;font-size:12px}`;
    // No script can run here (sandbox=""); base target keeps a clicked link
    // from replacing the output with the linked page.
    return `<!doctype html><html><head><meta charset="utf-8"><base target="_blank"><style>${css}</style></head><body>${html}</body></html>`;
  });

  // Offscreen layout of a sanitized copy with nothing that loads or styles:
  // only the structure (rows, paragraphs) decides the height.
  $effect(() => {
    const w = width;
    const src = html;
    if (w <= 0) return;
    const probe = document.createElement("div");
    probe.setAttribute("aria-hidden", "true");
    probe.style.cssText = `position:absolute;left:-100000px;top:0;width:${w}px;visibility:hidden;contain:layout style;font:${FONT};padding:4px 2px;box-sizing:border-box`;
    probe.appendChild(
      purify.sanitize(src, {
        RETURN_DOM_FRAGMENT: true,
        FORBID_TAGS: ["style", "img", "picture", "video", "audio", "iframe", "object", "embed", "svg", "link", "input", "form"],
        FORBID_ATTR: ["style", "src", "srcset", "background", "poster"],
      }),
    );
    const table = probe.querySelectorAll("table");
    for (const t of table) t.style.cssText = "border-collapse:collapse;font-size:12px";
    for (const c of probe.querySelectorAll<HTMLElement>("td, th")) c.style.cssText = "padding:3px 9px;border:1px solid transparent";
    document.body.appendChild(probe);
    natural = Math.ceil(probe.scrollHeight) + 10;
    probe.remove();
  });

  const height = $derived(
    natural === null
      ? 120
      : expanded
        ? Math.min(Math.max(natural, MIN_H), EXPANDED_MAX)
        : Math.min(Math.max(natural, MIN_H), MAX_H),
  );
</script>

<div class="nb-html" bind:clientWidth={width} style:height="{height}px">
  <iframe title="HTML output" sandbox="" srcdoc={doc} loading="lazy"></iframe>
</div>
{#if natural !== null && natural > MAX_H}
  <button class="expand" onclick={() => (expanded = !expanded)}>
    {expanded ? "collapse" : "show all"}
  </button>
{/if}

<style>
  .nb-html {
    position: relative;
    max-width: 100%;
    overflow: hidden;
    resize: vertical;
    min-height: 36px;
  }

  iframe {
    display: block;
    width: 100%;
    height: 100%;
    border: 0;
    background: transparent;
  }

  .expand {
    appearance: none;
    margin: 4px 0 2px;
    padding: 0.1rem 0.55rem;
    border: 1px solid var(--edge);
    border-radius: 5px;
    background: transparent;
    color: var(--muted);
    font: inherit;
    font-family: var(--ui-font);
    font-size: var(--text-xs);
    cursor: pointer;
  }

  .expand:hover {
    color: var(--fg);
    border-color: color-mix(in srgb, var(--fg) 30%, var(--edge));
  }
</style>
