<script lang="ts">
  /**
   * One notebook cell in a card (`#cell=N`, 1-based): its source,
   * highlighted, and its outputs drawn the way the notebook view draws them
   * — text and tracebacks as text, images inline, SVG only as an image,
   * HTML in NotebookHtml's script-less frame. Without a cell, the first cell
   * that drew a figure (else the first code cell): what a glance at a
   * notebook wants. One `fs/notebook` page; the daemon caps every output.
   */
  import DOMPurify from "dompurify";
  import { Marked } from "marked";
  import type { Parser } from "@lezer/common";
  import { stripAnsi } from "../../previews/ansi";
  import {
    base64Url,
    fsNotebook,
    pickMime,
    svgUrl,
    type NotebookCell,
    type NotebookOutput,
  } from "../../previews/notebook";
  import NotebookHtml from "../../previews/NotebookHtml.svelte";
  import { activateUrl, isWebUrl } from "../urlOpen";
  import { watchOverflow } from "./embed";

  interface Props {
    path: string;
    version: string;
    cell?: number;
    compact: boolean;
    active: boolean;
  }

  let { path, version, cell, compact, active }: Props = $props();

  /** Cells scanned for a figure when no cell is named. */
  const SCAN = 12;
  /** Source lines shown above the outputs of a picked (unnamed) cell. */
  const PEEK_SOURCE = 4;

  let shown = $state.raw<NotebookCell | null>(null);
  let language = $state<string | null>(null);
  let error = $state<string | null>(null);
  let expanded = $state(false);
  let srcEl = $state<HTMLElement | null>(null);
  let host = $state<HTMLElement | null>(null);
  /** The cell is taller than the card shows: offer "more", fade the cut. */
  let clipped = $state(false);
  $effect(() => {
    const el = host;
    void shown;
    void expanded;
    if (el === null) return;
    return watchOverflow(el, (v) => (clipped = v));
  });

  const marked = new Marked({ gfm: true, breaks: false });

  function drawsFigure(c: NotebookCell): boolean {
    return (c.outputs ?? []).some((o) => {
      const m = pickMime(o.data);
      return m !== null && m.startsWith("image/");
    });
  }

  let gen = 0;
  $effect(() => {
    const p = path;
    const n = cell;
    void version;
    if (!active) return;
    const mine = ++gen;
    error = null;
    const load = n !== undefined ? fsNotebook(p, n - 1, 1) : fsNotebook(p, 0, SCAN);
    load.then(
      (page) => {
        if (mine !== gen) return;
        language = page.language;
        if (n !== undefined) {
          shown = page.cells[0] ?? null;
          if (shown === null) error = `the notebook has ${page.total} cell${page.total === 1 ? "" : "s"}`;
          return;
        }
        shown =
          page.cells.find(drawsFigure) ??
          page.cells.find((c) => c.cell_type === "code") ??
          page.cells[0] ??
          null;
        if (shown === null) error = "an empty notebook";
      },
      (e: unknown) => {
        if (mine === gen) error = e instanceof Error ? e.message : "couldn't read this notebook";
      },
    );
  });

  const source = $derived.by(() => {
    if (shown === null) return "";
    if (cell !== undefined || expanded) return shown.source;
    const lines = shown.source.split("\n");
    return lines.length > PEEK_SOURCE ? `${lines.slice(0, PEEK_SOURCE).join("\n")}\n…` : shown.source;
  });

  // Highlight code cells with the notebook's language.
  $effect(() => {
    const el = srcEl;
    const text = source;
    const lang = language;
    if (el === null || shown?.cell_type !== "code") return;
    el.textContent = text;
    let stale = false;
    void import("../../previews/highlight")
      .then(async ({ parserFor, renderCode }) => {
        const parser: Parser | null = await parserFor(lang ?? "python");
        if (!stale && parser !== null) renderCode(el, text, parser);
      })
      .catch(() => {});
    return () => {
      stale = true;
    };
  });

  /** Markdown (cells, or text/markdown outputs): marked, then DOMPurify
   *  with no style and no remote or relative images (data: ones stay). */
  function renderMarkdown(md: string): string {
    const html = DOMPurify.sanitize(marked.parse(md, { async: false }) as string, {
      FORBID_TAGS: ["style"],
      FORBID_ATTR: ["style"],
    });
    const tpl = document.createElement("template");
    tpl.innerHTML = html;
    for (const img of tpl.content.querySelectorAll("img")) {
      if (!(img.getAttribute("src") ?? "").startsWith("data:")) img.remove();
    }
    return tpl.innerHTML;
  }

  function outputText(o: NotebookOutput): string {
    if (o.output_type === "stream") return stripAnsi(o.text ?? "");
    if (o.output_type === "error") {
      return stripAnsi(o.traceback ?? `${o.ename ?? "Error"}: ${o.evalue ?? ""}`);
    }
    return o.data?.["text/plain"] ?? "";
  }

  function onClick(e: MouseEvent): void {
    const a = (e.target as Element | null)?.closest?.("a[href]");
    if (a === null || a === undefined) return;
    e.preventDefault();
    const href = a.getAttribute("href") ?? "";
    if (isWebUrl(href)) activateUrl(href, e.metaKey || e.ctrlKey);
  }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="nb-body" class:tile={compact} class:open={expanded} class:clipped={clipped && !expanded} bind:this={host} onclick={onClick}>
  {#if error !== null}
    <div class="note">{error}</div>
  {:else if shown === null}
    <div class="note">{active ? "reading the notebook…" : ""}</div>
  {:else}
    <div class="cell-label">
      {#if shown.cell_type === "code"}
        In [{shown.execution_count ?? " "}] · cell {shown.index + 1}
      {:else}
        cell {shown.index + 1}
      {/if}
    </div>
    {#if shown.cell_type === "markdown"}
      <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized above -->
      <div class="md">{@html renderMarkdown(shown.source)}</div>
    {:else}
      <div class="src" bind:this={srcEl}></div>
    {/if}
    {#each shown.outputs ?? [] as o, i (i)}
      {@const mime = pickMime(o.data)}
      {#if o.output_type === "stream" || o.output_type === "error"}
        <div class="out text" class:err={o.output_type === "error" || o.name === "stderr"}>{outputText(o)}</div>
      {:else if mime === "image/png" || mime === "image/jpeg" || mime === "image/gif"}
        <img class="out fig" src={base64Url(mime, o.data?.[mime] ?? "")} alt="output {i + 1}" />
      {:else if mime === "image/svg+xml"}
        <img class="out fig" src={svgUrl(o.data?.[mime] ?? "")} alt="output {i + 1}" />
      {:else if mime === "text/html"}
        <div class="out"><NotebookHtml html={o.data?.[mime] ?? ""} /></div>
      {:else if mime === "text/markdown"}
        <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized above -->
        <div class="out md">{@html renderMarkdown(o.data?.[mime] ?? "")}</div>
      {:else if mime !== null}
        <div class="out text">{outputText(o)}</div>
      {/if}
    {/each}
    {#if !compact && (clipped || expanded)}
      <button class="more" onclick={() => (expanded = !expanded)}>{expanded ? "less" : "more"}</button>
    {/if}
  {/if}
</div>

<style>
  .nb-body {
    position: relative;
    max-height: 360px;
    overflow: hidden;
    padding: 6px 10px 10px;
  }
  .nb-body.open {
    max-height: none;
  }
  .nb-body.tile {
    flex: 1;
    max-height: none;
    min-height: 0;
  }
  /* A clipped cell fades out instead of ending mid-line. */
  .nb-body.clipped::after {
    content: "";
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    height: 36px;
    background: linear-gradient(to bottom, transparent, color-mix(in srgb, var(--fg) 2%, var(--bg)));
    pointer-events: none;
  }
  .cell-label {
    margin-bottom: 4px;
    color: var(--muted);
    font-family: var(--mono, monospace);
    font-size: var(--text-xs);
  }
  .src {
    padding: 6px 8px;
    border-radius: 4px;
    background: color-mix(in srgb, var(--term-bg) 60%, transparent);
    font-family: var(--mono, monospace);
    font-size: 12px;
    line-height: 1.5;
    white-space: pre;
    overflow-x: auto;
    scrollbar-width: thin;
  }
  .out {
    margin-top: 6px;
  }
  .out.text {
    font-family: var(--mono, monospace);
    font-size: 12px;
    line-height: 1.45;
    white-space: pre-wrap;
    word-break: break-word;
    color: var(--fg);
  }
  .out.text.err {
    color: var(--err);
  }
  .out.fig {
    display: block;
    max-width: 100%;
    max-height: 300px;
    object-fit: contain;
    border-radius: 3px;
    background: #ffffff;
  }
  .tile .out.fig {
    max-height: 150px;
  }
  .md {
    font-size: var(--text-sm);
    line-height: 1.5;
  }
  .md :global(p) {
    margin: 0.3em 0;
  }
  .md :global(a) {
    color: var(--accent);
  }
  .more {
    position: absolute;
    right: 10px;
    bottom: 8px;
    z-index: 1;
    padding: 1px 8px;
    border: 1px solid var(--edge);
    border-radius: 999px;
    background: var(--bg);
    color: var(--muted);
    font: inherit;
    font-size: var(--text-xs);
    cursor: pointer;
  }
  .more:hover {
    color: var(--accent);
    border-color: var(--accent);
  }
  .note {
    padding: 6px 2px;
    color: var(--muted);
    font-size: var(--text-xs);
  }
  .src :global(.hl-keyword) { color: var(--syn-keyword); }
  .src :global(.hl-string) { color: var(--syn-string); }
  .src :global(.hl-comment) { color: var(--syn-comment); font-style: italic; }
  .src :global(.hl-number) { color: var(--syn-number); }
  .src :global(.hl-type) { color: var(--syn-type); }
  .src :global(.hl-func) { color: var(--syn-func); }
  .src :global(.hl-def) { color: var(--syn-def); }
  .src :global(.hl-prop) { color: var(--syn-prop); }
  .src :global(.hl-invalid) { color: var(--err); }
</style>
