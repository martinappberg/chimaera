<script lang="ts">
  /**
   * One card for any embedded file — in a document, in agent prose, in a
   * turn's "made this turn" gallery. A thin header (file icon, name, the
   * piece shown, open in a pane, download on a remote host) over the file's
   * own compact viewer: an image (a `#xywh=` region cropped), a PDF page, a
   * code excerpt with line numbers, a table slice, a sandboxed HTML report,
   * a player, a notebook cell, a Marp slide, a markdown excerpt — or a plain
   * file card for everything else.
   *
   * Nothing loads until the card nears its scroller's viewport. Its box is
   * reserved from the start (image dimensions come with the resolve answer),
   * so nothing jumps as bytes arrive. While on screen the card watches its
   * file (the daemon's bounded disk monitor): an overwrite re-resolves it,
   * and the new version's `/raw` ticket is a new URL, so the fresh bytes
   * show; an unchanged file keeps its URL and the browser's cached copy.
   * Missing files and failed loads say so in place — never a blank box.
   */
  import FileIcon from "../FileIcon.svelte";
  import FolderIcon from "../FolderIcon.svelte";
  import { basename, formatMtime, fsDownload, humanSize } from "../../previews/files";
  import { isRemoteHost } from "../../net/api";
  import { openPath } from "../openPath";
  import type { Reveal } from "../reveal";
  import { lastDiskChange, releaseDiskFile, retainDiskFile } from "../../workspace/diskWatch";
  import {
    embedKind,
    isMissing,
    rawUrl,
    resolveFile,
    type TargetInfo,
    type TargetResult,
  } from "./embed";
  import { fragmentLabel, fragmentReveal, parseEmbedFragment } from "./fragment";
  import ImageBody from "./ImageBody.svelte";
  import PdfBody from "./PdfBody.svelte";
  import CodeBody from "./CodeBody.svelte";
  import TableBody from "./TableBody.svelte";
  import HtmlBody from "./HtmlBody.svelte";
  import MediaBody from "./MediaBody.svelte";
  import NotebookBody from "./NotebookBody.svelte";
  import MarkdownBody from "./MarkdownBody.svelte";

  interface Props {
    /** The file: its absolute path once resolved; before that (or when it
     *  is missing) the target as written, which the header shows. */
    path: string;
    /** The resolve answer (`resolveTargets`), when the host already has it. */
    info?: TargetResult | null;
    /** The piece to show (`page=3`, `L10-L30`, …), without the `#`. */
    fragment?: string | null;
    /** Caption / alt text. */
    alt?: string;
    /** The author's width hint in px (`![x|400](…)`). */
    width?: number | null;
    /** Gallery tile: a fixed, shorter body. */
    compact?: boolean;
    /** How to resolve when no `info` came with the card (chat prose embeds
     *  resolve against the session's directories). Default: `path` as an
     *  absolute path. Null answer = unknown (the daemon did not answer). */
    resolve?: () => Promise<TargetResult | null>;
    /** Open in a pane. Default: the workbench opener (`openPath`). */
    onOpen?: (path: string, kind: "file" | "dir", reveal?: Reveal) => void;
  }

  let {
    path,
    info = null,
    fragment = null,
    alt = "",
    width = null,
    compact = false,
    resolve,
    onOpen,
  }: Props = $props();

  /** A fresher answer than `info` (a self-resolve, or a refresh after the
   *  file changed on disk). Cleared when the host hands a new `info`. */
  let fetched = $state<TargetResult | null>(null);
  let failed = $state<string | null>(null);
  let resolving = false;
  $effect(() => {
    void info;
    fetched = null;
    failed = null;
  });
  const current = $derived<TargetResult | null>(fetched ?? info);
  const hit = $derived<TargetInfo | null>(current !== null && !isMissing(current) ? current : null);
  const kind = $derived(hit !== null ? embedKind(hit) : null);
  const name = $derived(basename(hit?.path ?? path) || path);
  /** The piece to show, read as links and references read it (`cell=`
   *  depends on the file's extension). */
  const frag = $derived(parseEmbedFragment(fragment, hit?.path ?? path));
  const label = $derived(fragmentLabel(frag));

  let card = $state<HTMLElement | null>(null);
  /** Near the scroller's viewport: load (one-way — a loaded body stays). */
  let near = $state(false);
  /** Actually on screen: watch the file for changes. */
  let onScreen = $state(false);

  /** The nearest scrolling ancestor — the observers' root, so the preload
   *  margin extends past the scroller's edge rather than the window's. */
  function scrollParent(el: HTMLElement): Element | null {
    for (let p = el.parentElement; p !== null; p = p.parentElement) {
      const oy = getComputedStyle(p).overflowY;
      if (oy === "auto" || oy === "scroll") return p;
    }
    return null;
  }

  $effect(() => {
    const el = card;
    if (el === null) return;
    if (typeof IntersectionObserver === "undefined") {
      near = true;
      onScreen = true;
      return;
    }
    const root = scrollParent(el);
    const nearing = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) {
          near = true;
          nearing.disconnect();
        }
      },
      { root, rootMargin: "480px 0px" },
    );
    const seeing = new IntersectionObserver((entries) => {
      const last = entries[entries.length - 1];
      if (last !== undefined) onScreen = last.isIntersecting;
    }, { root });
    nearing.observe(el);
    seeing.observe(el);
    return () => {
      nearing.disconnect();
      seeing.disconnect();
    };
  });

  function defaultResolve(): Promise<TargetResult | null> {
    return path.startsWith("/") ? resolveFile(path) : Promise.resolve(null);
  }

  async function load(): Promise<void> {
    if (resolving) return;
    resolving = true;
    failed = null;
    try {
      const r = await (resolve ?? defaultResolve)();
      if (r === null) failed = "couldn't reach the daemon";
      else fetched = r;
    } catch (e) {
      failed = e instanceof Error ? e.message : "couldn't resolve this file";
    } finally {
      resolving = false;
    }
  }

  // Resolve once near, when the host did not already.
  $effect(() => {
    if (!near || current !== null || failed !== null) return;
    void load();
  });

  /** A missing file may be written after the prose that embeds it (an
   *  agent announces the plot, then saves it): look again whenever the card
   *  comes back on screen, at most every few seconds. */
  const canRecheck = $derived(resolve !== undefined || path.startsWith("/"));
  let checkedAt = 0;
  $effect(() => {
    if (!onScreen || current === null || !isMissing(current) || !canRecheck) return;
    if (Date.now() - checkedAt < 8000) return;
    checkedAt = Date.now();
    void load();
  });

  // While on screen, watch the file: an overwrite re-resolves it (a new
  // version is a new ticket, so the body reloads fresh bytes); a delete
  // turns the card into its missing state. Registered only while visible —
  // the daemon's watch list is small (64 paths) and shared with open panes.
  /** A primitive, so a refresh to a new version (same path) keeps the
   *  registration instead of dropping and re-adding it. */
  const watchedPath = $derived(hit?.path ?? null);
  $effect(() => {
    const target = watchedPath;
    if (!onScreen || target === null) return;
    retainDiskFile(target);
    let first = true;
    let seen = 0;
    const stop = lastDiskChange.subscribe((change) => {
      if (first) {
        first = false;
        seen = change?.seq ?? 0;
        return;
      }
      if (change === null || change.seq === seen) return;
      seen = change.seq;
      if (change.removed.includes(target)) fetched = { missing: true };
      else if (change.files.includes(target)) {
        void resolveFile(target).then((r) => {
          if (r !== null && hit?.path === target) fetched = r;
        });
      }
    });
    return () => {
      stop();
      releaseDiskFile(target);
    };
  });

  const reveal = $derived(fragmentReveal(frag));
  const remote = isRemoteHost();

  function open(): void {
    const p = hit?.path;
    if (p === undefined || hit === null) return;
    if (onOpen !== undefined) onOpen(p, hit.kind, reveal);
    else openPath(p, hit.kind, reveal !== undefined ? { reveal } : {});
  }

  function download(): void {
    if (hit !== null) void fsDownload(hit.path);
  }

  const url = $derived(hit !== null ? rawUrl(hit) : null);
  const facts = $derived.by(() => {
    if (hit === null) return "";
    const bits: string[] = [];
    if (hit.kind === "file") bits.push(humanSize(hit.size));
    if (hit.mtime_ms !== null) bits.push(`modified ${formatMtime(hit.mtime_ms / 1000)}`);
    return bits.join(" · ");
  });
</script>

<div
  class="embed-card"
  class:compact
  class:fit={kind === "image" && !compact}
  class:missing={current !== null && isMissing(current)}
  bind:this={card}
  data-embed-kind={kind ?? "pending"}
>
  <div class="head">
    <button class="name" title={hit !== null ? `open ${hit.path} in a pane` : path} disabled={hit === null} onclick={open}>
      {#if kind === "dir"}
        <FolderIcon size={13} />
      {:else}
        <FileIcon path={hit?.path ?? path} size={13} />
      {/if}
      <span class="label">{name}</span>
    </button>
    {#if label !== ""}<span class="frag" title={fragment ?? ""}>{label}</span>{/if}
    <span class="spacer"></span>
    {#if hit !== null}
      {#if remote && hit.kind === "file"}
        <button class="act" title="download to this computer" aria-label="download {name}" onclick={download}>
          <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true"
            ><path d="M8 2.5v8M4.5 7.5 8 11l3.5-3.5M3 13.5h10" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" /></svg
          >
        </button>
      {/if}
      <button class="act" title="open in a pane" aria-label="open {name} in a pane" onclick={open}>
        <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true"
          ><path d="M9.5 2.5h4v4M13.5 2.5 8 8M6.5 3.5h-3v9h9v-3" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" /></svg
        >
      </button>
    {/if}
  </div>

  {#if current === null}
    <div class="state" class:tile={compact}>
      {#if failed !== null}
        <span>{failed}</span>
        <button class="retry" onclick={() => void load()}>try again</button>
      {:else}
        <span class="quiet">{near ? "loading…" : ""}</span>
      {/if}
    </div>
  {:else if hit === null}
    <div class="state gone" class:tile={compact}>
      <span>Not found: <code>{path}</code></span>
      <span class="quiet">nothing at this path — it may not have been written yet, or it moved</span>
      {#if canRecheck}
        <button class="retry" onclick={() => { checkedAt = Date.now(); void load(); }}>check again</button>
      {/if}
    </div>
  {:else if kind === "image"}
    <ImageBody
      {url}
      natural={hit.width !== undefined && hit.height !== undefined ? { w: hit.width, h: hit.height } : null}
      region={frag.at?.region}
      alt={alt || name}
      hint={width}
      {compact}
      active={near}
      onOpen={open}
    />
  {:else if kind === "pdf"}
    <PdfBody {url} page={frag.at?.page ?? 1} region={frag.at?.region} {compact} active={near} onOpen={open} />
  {:else if kind === "code"}
    <CodeBody path={hit.path} version={hit.version} lines={frag.lines} {compact} active={near} />
  {:else if kind === "table" || kind === "xlsx"}
    <TableBody path={hit.path} version={hit.version} {kind} {frag} {compact} active={near} />
  {:else if kind === "html"}
    <HtmlBody {url} title={name} {compact} active={near} />
  {:else if kind === "video" || kind === "audio"}
    <MediaBody {url} {kind} time={frag.at?.time} {compact} active={near} onDownload={remote ? download : undefined} />
  {:else if kind === "notebook"}
    <NotebookBody path={hit.path} version={hit.version} cell={frag.at?.cell} {compact} active={near} />
  {:else if kind === "markdown"}
    <MarkdownBody
      path={hit.path}
      version={hit.version}
      anchor={frag.anchor}
      slide={frag.at?.slide}
      {compact}
      active={near}
      onOpen={open}
    />
  {:else}
    <div class="state file" class:tile={compact}>
      {#if kind === "dir"}
        <FolderIcon size={28} />
      {:else}
        <FileIcon path={hit.path} size={28} />
      {/if}
      <span class="facts">{facts}</span>
    </div>
  {/if}
  {#if alt !== "" && kind !== null && kind !== "image" && !compact}
    <div class="caption">{alt}</div>
  {/if}
</div>

<style>
  .embed-card {
    display: flex;
    flex-direction: column;
    min-width: 0;
    max-width: 100%;
    margin: 0.5em 0;
    border: 1px solid color-mix(in srgb, var(--edge) 75%, transparent);
    border-radius: 8px;
    background: color-mix(in srgb, var(--fg) 2%, var(--bg));
    overflow: hidden;
    /* Agent prose around the card must not restyle it. */
    font-size: var(--text-sm);
    line-height: 1.45;
    color: var(--fg);
    text-align: left;
    white-space: normal;
  }
  .embed-card.compact {
    margin: 0;
    height: 100%;
  }
  /* A picture's card hugs the picture (a figure, not a banner). */
  .embed-card.fit {
    width: fit-content;
    min-width: min(240px, 100%);
  }
  .embed-card.missing {
    border-style: dashed;
    background: transparent;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 6px;
    min-height: 28px;
    padding: 0 4px 0 6px;
    border-bottom: 1px solid color-mix(in srgb, var(--edge) 55%, transparent);
  }
  .missing .head {
    border-bottom-style: dashed;
  }
  .name {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    padding: 3px 4px;
    border: none;
    border-radius: 4px;
    background: none;
    color: var(--fg);
    font: inherit;
    font-family: var(--mono, monospace);
    font-size: var(--text-xs);
    cursor: pointer;
    transition:
      color 0.12s ease,
      background-color 0.12s ease;
  }
  .name:hover:not(:disabled) {
    color: var(--accent);
    background: color-mix(in srgb, var(--fg) 5%, transparent);
  }
  .name:disabled {
    cursor: default;
    color: var(--muted);
  }
  .label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .frag {
    flex: none;
    max-width: 40%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    padding: 1px 6px;
    border-radius: 999px;
    background: color-mix(in srgb, var(--accent) 10%, transparent);
    color: color-mix(in srgb, var(--accent) 80%, var(--fg));
    font-size: var(--text-xs);
  }
  .spacer {
    flex: 1;
  }
  .act {
    flex: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 22px;
    border: none;
    border-radius: 4px;
    background: none;
    color: var(--muted);
    cursor: pointer;
    transition:
      color 0.12s ease,
      background-color 0.12s ease;
  }
  .act:hover {
    color: var(--accent);
    background: color-mix(in srgb, var(--fg) 6%, transparent);
  }
  .state {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    justify-content: center;
    gap: 4px;
    min-height: 64px;
    padding: 10px 12px;
    color: var(--fg);
  }
  .state.tile {
    flex: 1;
    min-height: 120px;
  }
  .state.gone {
    color: var(--muted);
  }
  .state.gone code {
    font-family: var(--mono, monospace);
    font-size: 0.95em;
    color: var(--fg);
  }
  .state.file {
    flex-direction: row;
    align-items: center;
    justify-content: flex-start;
    gap: 10px;
    min-height: 56px;
    color: var(--muted);
  }
  .state.file.tile {
    flex-direction: column;
    justify-content: center;
    align-items: center;
  }
  .facts {
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .quiet {
    color: var(--muted);
    font-size: var(--text-xs);
  }
  .retry {
    padding: 2px 8px;
    border: 1px solid var(--edge);
    border-radius: 4px;
    background: none;
    color: var(--fg);
    font: inherit;
    font-size: var(--text-xs);
    cursor: pointer;
  }
  .retry:hover {
    color: var(--accent);
    border-color: var(--accent);
  }
  .caption {
    padding: 4px 10px 6px;
    border-top: 1px solid color-mix(in srgb, var(--edge) 45%, transparent);
    color: var(--muted);
    font-size: var(--text-xs);
  }
</style>
