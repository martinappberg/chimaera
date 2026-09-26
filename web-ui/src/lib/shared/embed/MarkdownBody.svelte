<script lang="ts">
  /**
   * A markdown document in a card: a Marp deck shows a slide (`#slide=N`,
   * else the first); any other document shows an excerpt — its opening, or
   * the section a `#heading` fragment names — rendered like chat prose
   * (marked, then DOMPurify: no style, no images, links never navigate the
   * workbench) and clipped with a fade. One `fs/file` read of the head
   * (a deck's whole source, up to 2 MB, when the head says Marp).
   */
  import DOMPurify from "dompurify";
  import { Marked } from "marked";
  import { fsFile } from "../../previews/files";
  import { frontmatterOf, isMarpSource } from "../../previews/marp";
  import { activateUrl, isWebUrl } from "../urlOpen";
  import SlideBody from "./SlideBody.svelte";
  import { watchOverflow } from "./embed";

  interface Props {
    path: string;
    version: string;
    anchor?: string;
    slide?: number;
    compact: boolean;
    active: boolean;
    onOpen: () => void;
  }

  let { path, version, anchor, slide, compact, active, onOpen }: Props = $props();

  const HEAD = 64 * 1024;
  const DECK = 2 * 1024 * 1024;
  /** Source lines rendered for an opening excerpt (the clip does the rest). */
  const EXCERPT_LINES = 80;

  let source = $state<string | null>(null);
  let deck = $state(false);
  let error = $state<string | null>(null);
  let expanded = $state(false);
  let host = $state<HTMLElement | null>(null);
  /** The excerpt is taller than the card shows: offer "more", fade the cut. */
  let clipped = $state(false);
  $effect(() => {
    const el = host;
    void view;
    void expanded;
    if (el === null) return;
    return watchOverflow(el, (v) => (clipped = v));
  });

  const marked = new Marked({ gfm: true, breaks: false });

  let gen = 0;
  $effect(() => {
    const p = path;
    void version;
    if (!active) return;
    const mine = ++gen;
    error = null;
    void (async () => {
      let chunk = await fsFile(p, 0, HEAD);
      let text = new TextDecoder().decode(chunk.bytes);
      const marp = isMarpSource(text);
      if (marp && chunk.truncated) {
        chunk = await fsFile(p, 0, DECK);
        if (chunk.truncated) throw new Error("this deck is over 2 MB — open it to read");
        text = new TextDecoder().decode(chunk.bytes);
      }
      if (mine !== gen) return;
      deck = marp;
      source = text;
    })().catch((e: unknown) => {
      if (mine === gen) error = e instanceof Error ? e.message : "couldn't read this document";
    });
  });

  /** GitHub's heading slug (lowercase, punctuation dropped, spaces → -). */
  function slug(s: string): string {
    return s
      .trim()
      .toLowerCase()
      .replace(/[^\p{L}\p{N}\s_-]/gu, "")
      .replace(/\s/g, "-");
  }

  /** The lines an excerpt renders: the section `anchor` names (its heading
   *  through the next heading of the same or a higher rank), else the
   *  document's opening after any frontmatter. Fences are skipped when
   *  looking for headings. */
  function excerpt(text: string, want: string | undefined): { md: string; found: boolean } {
    let lines = text.split(/\r?\n/);
    const fm = frontmatterOf(text);
    if (fm !== null) lines = lines.slice(fm === "" ? 2 : fm.split("\n").length + 2);
    if (want === undefined) return { md: lines.slice(0, EXCERPT_LINES).join("\n"), found: true };
    const target = slug(want.replace(/^user-content-/, ""));
    let fence: string | null = null;
    let start = -1;
    let level = 0;
    for (let i = 0; i < lines.length; i++) {
      const line = lines[i];
      const f = /^\s{0,3}(`{3,}|~{3,})/.exec(line);
      if (f !== null) {
        if (fence === null) fence = f[1][0];
        else if (f[1][0] === fence) fence = null;
        continue;
      }
      if (fence !== null) continue;
      const h = /^(#{1,6})\s+(.*?)\s*#*\s*$/.exec(line);
      if (h === null) continue;
      if (start >= 0 && h[1].length <= level) return { md: lines.slice(start, i).join("\n"), found: true };
      if (start < 0 && slug(h[2]) === target) {
        start = i;
        level = h[1].length;
      }
    }
    if (start >= 0) return { md: lines.slice(start, start + EXCERPT_LINES * 2).join("\n"), found: true };
    return { md: lines.slice(0, EXCERPT_LINES).join("\n"), found: false };
  }

  const view = $derived.by(() => {
    if (source === null || deck) return null;
    const { md, found } = excerpt(source, anchor);
    const html = DOMPurify.sanitize(marked.parse(md, { async: false }) as string, {
      FORBID_TAGS: ["style", "img", "picture", "video", "audio", "iframe", "form", "input"],
      FORBID_ATTR: ["style"],
    });
    return { html, found };
  });

  function onClick(e: MouseEvent): void {
    const a = (e.target as Element | null)?.closest?.("a[href]");
    if (a === null || a === undefined) return;
    e.preventDefault();
    const href = a.getAttribute("href") ?? "";
    if (isWebUrl(href)) activateUrl(href, e.metaKey || e.ctrlKey);
    else onOpen();
  }
</script>

{#if error !== null}
  <div class="md-body note-only"><div class="note">{error}</div></div>
{:else if deck && source !== null}
  <SlideBody {path} {source} slide={slide ?? 1} {compact} />
{:else}
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div
    class="md-body"
    class:tile={compact}
    class:open={expanded}
    class:clipped={clipped && !expanded}
    bind:this={host}
    onclick={onClick}
  >
    {#if view === null}
      <div class="note">{active ? "reading…" : ""}</div>
    {:else}
      {#if !view.found}<div class="note">no heading “{anchor}” — showing the opening</div>{/if}
      <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized above -->
      <div class="prose">{@html view.html}</div>
      {#if !compact && (clipped || expanded)}
        <button class="more" onclick={(e) => { e.stopPropagation(); expanded = !expanded; }}>{expanded ? "less" : "more"}</button>
      {/if}
    {/if}
  </div>
{/if}

<style>
  .md-body {
    position: relative;
    max-height: 260px;
    overflow: hidden;
    padding: 4px 14px 10px;
  }
  .md-body.open {
    max-height: 1200px;
    overflow: auto;
  }
  .md-body.tile {
    flex: 1;
    max-height: none;
    min-height: 0;
    padding: 2px 12px 8px;
  }
  .md-body.clipped::after {
    content: "";
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    height: 40px;
    background: linear-gradient(to bottom, transparent, color-mix(in srgb, var(--fg) 2%, var(--bg)));
    pointer-events: none;
  }
  .prose {
    font-size: var(--text-sm);
    line-height: 1.55;
    word-break: break-word;
  }
  .tile .prose {
    font-size: var(--text-xs);
  }
  .prose :global(h1),
  .prose :global(h2),
  .prose :global(h3),
  .prose :global(h4) {
    margin: 0.6em 0 0.3em;
    font-size: 1.05em;
    font-weight: 600;
    line-height: 1.3;
  }
  .prose :global(h1) {
    font-size: 1.2em;
  }
  .prose :global(p),
  .prose :global(ul),
  .prose :global(ol) {
    margin: 0.35em 0;
  }
  .prose :global(ul),
  .prose :global(ol) {
    padding-left: 1.3em;
  }
  .prose :global(code) {
    font-family: var(--mono, monospace);
    font-size: 0.92em;
    padding: 0.05em 0.3em;
    border-radius: 3px;
    background: color-mix(in srgb, var(--fg) 7%, transparent);
  }
  .prose :global(pre) {
    padding: 6px 8px;
    border-radius: 4px;
    background: color-mix(in srgb, var(--fg) 5%, transparent);
    overflow-x: auto;
  }
  .prose :global(pre code) {
    padding: 0;
    background: none;
  }
  .prose :global(a) {
    color: var(--accent);
  }
  .prose :global(table) {
    border-collapse: collapse;
    font-size: 0.95em;
  }
  .prose :global(th),
  .prose :global(td) {
    padding: 2px 8px;
    border: 1px solid color-mix(in srgb, var(--edge) 70%, transparent);
  }
  .prose :global(blockquote) {
    margin: 0.4em 0;
    padding: 2px 10px;
    border-left: 2px solid color-mix(in srgb, var(--accent) 60%, transparent);
    color: var(--muted);
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
    padding: 6px 0;
    color: var(--muted);
    font-size: var(--text-xs);
  }
</style>
