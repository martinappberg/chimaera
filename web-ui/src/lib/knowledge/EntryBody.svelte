<script lang="ts">
  /**
   * An entry's markdown, as written: the lines its span names, fetched from
   * the file and drawn by the reading renderer; ids inside become chips.
   * Without a span (an older provider, a to-do that is one table row) it
   * draws `fallback` — the provider's own fields, still markdown.
   */
  import { untrack } from "svelte";
  import { followDocHref, requestAnchor, showLinkHint } from "../previews/docLinks";
  import { openPath, pathPaneFrom } from "../shared/openPath";
  import { activeTheme } from "../settings/store.svelte";
  import type { Span } from "../workspace/knowledge";
  import { linkReferences, type RefMatcher, type ReferenceChips } from "../shared/references";
  import type { Entry } from "./entries";
  import { drawMarkdown, entryMarkdown, entrySource } from "./render";

  interface Props {
    span: Span | null;
    fallback: string;
    /** The entry it belongs to (a reused id resolves to its topic first). */
    from?: Entry;
    /** Ids to chips (the references registry's matcher; null: none). */
    matcher: RefMatcher | null;
    chips: ReferenceChips;
    wsRoot: string | null;
    wsId: string | null;
    /** The snapshot object: the file cache lives as long as it does. */
    owner: unknown;
    /** Drop the span's first line when it is the heading the reader shows. */
    dropHeading?: boolean;
  }

  let { span, fallback, from, matcher, chips, wsRoot, wsId, owner, dropHeading = true }: Props = $props();

  let box = $state<HTMLDivElement | null>(null);
  let problem = $state<string | null>(null);

  function abs(p: string): string {
    return p.startsWith("/") || wsRoot === null ? p : `${wsRoot}/${p}`;
  }

  /** A link in the body is routed, never a native navigation (that would
   *  replace the whole workbench): a web URL opens in a browser, a file in
   *  the workbench (Cmd/Ctrl beside), resolved against the entry's file. */
  function follow(e: MouseEvent, split: boolean): void {
    const a = (e.target as Element | null)?.closest?.("a[href]");
    if (a === null || a === undefined || box?.contains(a) !== true || a.closest(".embed-card") !== null) return;
    const href = a.getAttribute("href") ?? "";
    if (/^(mailto|tel):/i.test(href)) return;
    e.preventDefault();
    const docPath = abs(span?.path ?? "");
    const fromPane = pathPaneFrom(a);
    const x = e.clientX;
    const y = e.clientY;
    void followDocHref(href, split, {
      docPath,
      fromPane,
      wsRoot,
      workspaceId: wsId,
      toAnchor: (anchor) => {
        requestAnchor(docPath, anchor);
        return openPath(docPath, "file", { fromPane });
      },
      toLines: (r) => {
        openPath(docPath, "file", { reveal: r, fromPane });
      },
      hint: (text) => {
        if (box !== null) showLinkHint(box, x, y, text);
      },
    });
  }

  function onAux(e: MouseEvent): void {
    if (e.button !== 1) return;
    const href = (e.target as Element | null)?.closest?.("a[href]")?.getAttribute("href") ?? "";
    if (/^https?:/i.test(href)) return;
    follow(e, true);
  }

  $effect(() => {
    const el = box;
    const s = span;
    const theme = activeTheme().kind;
    const fb = fallback;
    if (el === null) return;
    let cancelled = false;
    let teardown: (() => void) | null = null;
    const draw = (markdown: string, docPath: string): void => {
      teardown?.();
      teardown = drawMarkdown(el, {
        docPath,
        markdown,
        links: () => ({ wsRoot, workspaceId: wsId }),
        theme,
      });
      untrack(() => {
        if (matcher !== null) linkReferences(el, matcher, chips, from);
      });
    };
    problem = null;
    // A one-line span (a to-do's table row) is no document of its own.
    if (s === null || (s.end_line > 0 && s.end_line <= s.line)) {
      draw(fb, abs(s?.path ?? ""));
    } else {
      const path = abs(s.path);
      entrySource(path, owner).then(
        (source) => {
          if (cancelled) return;
          const md = entryMarkdown(source, s.line, s.end_line, dropHeading);
          draw(md.trim() !== "" ? md : fb, path);
        },
        (e: unknown) => {
          if (cancelled) return;
          problem = `Couldn't read ${s.path}${e instanceof Error ? ` — ${e.message}` : ""}`;
          draw(fb, path);
        },
      );
    }
    return () => {
      cancelled = true;
      teardown?.();
    };
  });
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="body" bind:this={box} onclick={(e) => follow(e, e.metaKey || e.ctrlKey)} onauxclick={onAux}></div>
{#if problem !== null}<p class="problem">{problem}</p>{/if}

<style>
  .body {
    position: relative;
    font-size: 14.5px;
    line-height: 1.62;
    color: var(--fg);
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .problem {
    margin: 6px 0 0;
    font-size: var(--text-xs);
    color: var(--muted);
  }
  /* The reading view's rules for what a body draws (MarkdownView scopes its
     own; HoverPreview carries the same set for chat). */
  .body :global(.md-doc > :first-child) {
    margin-top: 0;
  }
  .body :global(.md-doc > :last-child) {
    margin-bottom: 0;
  }
  .body :global(.md-doc :is(h1, h2, h3, h4, h5, h6)) {
    line-height: 1.3;
    margin: 1.3em 0 0.45em;
    font-weight: 600;
    letter-spacing: -0.005em;
  }
  .body :global(.md-doc h1),
  .body :global(.md-doc h2) {
    font-size: 1.12em;
  }
  .body :global(.md-doc :is(h3, h4, h5, h6)) {
    font-size: 1em;
  }
  .body :global(.md-doc p) {
    margin: 0.6em 0;
  }
  .body :global(.md-doc :is(ul, ol)) {
    margin: 0.5em 0;
    padding-left: 1.4em;
  }
  .body :global(.md-doc li) {
    margin: 0.2em 0;
  }
  .body :global(.md-doc a) {
    color: var(--accent);
    text-decoration: none;
  }
  .body :global(.md-doc a:hover) {
    text-decoration: underline;
  }
  .body :global(.md-doc code) {
    font-family: var(--mono);
    font-size: 0.84em;
    background: color-mix(in srgb, var(--fg) 6%, transparent);
    border-radius: 4px;
    padding: 0.1em 0.34em;
  }
  .body :global(.md-doc pre) {
    background: color-mix(in srgb, var(--fg) 4.5%, transparent);
    border: 1px solid var(--edge);
    border-radius: 8px;
    padding: 0.7em 0.9em;
    overflow: hidden;
    line-height: 1.5;
  }
  .body :global(.md-doc pre code) {
    display: block;
    overflow-x: auto;
    background: none;
    padding: 0;
    font-size: 0.84em;
  }
  .body :global(.md-doc blockquote) {
    margin: 0.7em 0;
    padding: 0 0 0 0.9em;
    border-left: 3px solid var(--edge);
    color: var(--muted);
  }
  .body :global(.md-doc hr) {
    border: 0;
    border-top: 1px solid var(--edge);
    margin: 1.2em 0;
  }
  .body :global(.md-doc strong) {
    font-weight: 600;
  }
  .body :global(.md-doc img) {
    max-width: 100%;
  }
</style>
