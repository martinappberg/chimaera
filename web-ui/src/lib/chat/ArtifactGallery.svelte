<script lang="ts">
  /**
   * What a turn made, after its closing prose. The files its edit tools
   * wrote (absolute paths from the tools, so a tile opens whatever the prose
   * called the file), plus the files its shell commands wrote — the paths
   * its commands, outputs and prose mention that the daemon confirms exist
   * and were modified during the turn (artifacts.ts).
   *
   * Two shapes, by what a file is for (artifactShape): a *visual* — a
   * figure, a rendered report, a PDF, a clip — is looked at, so it gets an
   * embed tile up front; a *document* — a markdown note, a table, a
   * notebook, a deck — is opened, so it gets one chip on a quiet line, and
   * its tile only when the line is unfolded. A turn that only touched
   * documents therefore costs one line, not a wall of excerpts. Tiles load
   * near the viewport, stay fresh when a file is overwritten, and say so
   * when one is gone.
   */
  import { basename } from "../previews/files";
  import Chevron from "../shared/Chevron.svelte";
  import FileIcon from "../shared/FileIcon.svelte";
  import EmbedCard from "../shared/embed/EmbedCard.svelte";
  import { isMissing, type TargetInfo, type TargetResult } from "../shared/embed/embed";
  import type { OpenPathOptions, PathKind } from "../shared/openPath";
  import { artifactShape, writtenDuring } from "./artifacts";
  import type { EmbedResolver } from "./embeds";

  interface Props {
    /** Files the turn's tools reported writing (absolute). */
    paths: string[];
    /** Artifact-shaped paths the turn mentioned (as written). */
    mentioned?: string[];
    startedAtMs?: number | null;
    endedAtMs?: number | null;
    /** Resolves mentioned paths against the session's directories. */
    resolver?: EmbedResolver;
    onOpenPath?: (path: string, kind: PathKind, opts?: OpenPathOptions) => void;
  }

  let {
    paths,
    mentioned = [],
    startedAtMs = null,
    endedAtMs = null,
    resolver,
    onOpenPath,
  }: Props = $props();

  /** Tiles per shape: past this the turn made a directory's worth. */
  const MAX_TILES = 8;
  /** Chips on the files line before the rest fold behind "+n more". */
  const MAX_CHIPS = 6;

  let host = $state<HTMLElement | null>(null);
  let near = $state(false);
  /** Mentioned files confirmed written during the turn. */
  let confirmed = $state.raw<TargetInfo[]>([]);
  /** The documents' tiles, unfolded by the reader. */
  let peek = $state(false);
  let allChips = $state(false);

  $effect(() => {
    const el = host;
    if (el === null || near) return;
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

  // One resolve round trip for every mention, once the gallery is near.
  $effect(() => {
    const r = resolver;
    const wanted = mentioned;
    const start = startedAtMs;
    const end = endedAtMs;
    if (!near || r === undefined || wanted.length === 0 || start === null) return;
    let stale = false;
    void Promise.all(wanted.map((m) => r.resolve(m).catch((): TargetResult | null => null))).then((answers) => {
      if (stale) return;
      const known = new Set(paths);
      const out: TargetInfo[] = [];
      for (const a of answers) {
        if (a === null || isMissing(a) || a.kind !== "file" || known.has(a.path)) continue;
        if (!writtenDuring(a.mtime_ms, start, end)) continue;
        known.add(a.path);
        out.push(a);
      }
      confirmed = out;
    });
    return () => {
      stale = true;
    };
  });

  type Tile = { path: string; info: TargetInfo | null };

  const all = $derived.by((): Tile[] => {
    const out: Tile[] = paths.map((p) => ({ path: p, info: null }));
    for (const c of confirmed) out.push({ path: c.path, info: c });
    return out;
  });
  const visuals = $derived(all.filter((t) => artifactShape(t.path) === "visual").slice(0, MAX_TILES));
  const documents = $derived(all.filter((t) => artifactShape(t.path) === "document").slice(0, MAX_TILES * 3));
  /** The fold previews the first tiles' worth; the chips still name them all. */
  const previewed = $derived(documents.slice(0, MAX_TILES));
  const chips = $derived(allChips || documents.length <= MAX_CHIPS ? documents : documents.slice(0, MAX_CHIPS));

  function open(path: string, kind: "file" | "dir", reveal?: import("../shared/reveal").Reveal): void {
    onOpenPath?.(path, kind, reveal !== undefined ? { reveal } : {});
  }
</script>

{#snippet tiles(items: Tile[], label: string)}
  <div class="gallery" role="group" aria-label={label}>
    {#each items as tile (tile.path)}
      <div class="tile">
        <EmbedCard
          path={tile.path}
          info={tile.info}
          compact
          onOpen={onOpenPath !== undefined ? open : undefined}
        />
      </div>
    {/each}
  </div>
{/snippet}

<div class="gallery-host" bind:this={host}>
  {#if visuals.length > 0}
    <div class="label">Made this turn</div>
    {@render tiles(visuals, "made this turn")}
  {/if}
  {#if documents.length > 0}
    <!-- Documents are opened, not stared at: one chip each, the same quiet
         voice as a folded activity line, and their tiles behind a fold. -->
    <div class="files" role="group" aria-label="files this turn">
      <span class="files-label">Files</span>
      {#each chips as doc (doc.path)}
        <button
          class="chip"
          title={onOpenPath !== undefined ? `open ${doc.path}` : doc.path}
          disabled={onOpenPath === undefined}
          onclick={() => open(doc.path, "file")}
        >
          <FileIcon path={doc.path} size={13} />
          <span class="name">{basename(doc.path)}</span>
        </button>
      {/each}
      {#if chips.length < documents.length}
        <button class="more" onclick={() => (allChips = true)}>+{documents.length - chips.length} more</button>
      {/if}
      <button
        class="peek"
        aria-expanded={peek}
        title={peek ? "hide the previews" : "preview these files here"}
        onclick={() => (peek = !peek)}
      >
        preview
        <Chevron open={peek} />
      </button>
    </div>
    {#if peek}
      {@render tiles(previewed, "files this turn, previewed")}
    {/if}
  {/if}
</div>

<style>
  .gallery-host {
    min-height: 1px;
  }
  .label {
    margin: 8px 0 4px;
    color: var(--activity-fg, var(--muted));
    font-size: var(--text-xs);
  }
  .gallery {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(260px, 100%), 1fr));
    gap: 10px;
    margin: 0 0 10px;
  }
  .tile {
    display: flex;
    min-width: 0;
    height: 252px;
  }
  .tile > :global(.embed-card) {
    flex: 1;
  }
  .files {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 6px;
    margin: 6px 0 8px;
    color: var(--activity-fg, var(--muted));
    font-size: var(--text-xs);
    line-height: 1.4;
  }
  .files-label {
    margin-right: 2px;
  }
  .chip,
  .more,
  .peek {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    max-width: 100%;
    padding: 1px 7px 1px 5px;
    border: 1px solid var(--edge);
    border-radius: 999px;
    background: color-mix(in srgb, var(--fg) 3%, transparent);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-xs);
    line-height: 1.5;
    cursor: pointer;
    transition:
      border-color 0.12s ease,
      background-color 0.12s ease,
      color 0.12s ease;
  }
  .chip:hover:not(:disabled),
  .chip:focus-visible {
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
    background: color-mix(in srgb, var(--accent) 9%, transparent);
  }
  .chip:disabled {
    cursor: default;
  }
  .chip .name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono, monospace);
  }
  /* "+n more" and the preview fold: text-only, no card edge. */
  .more,
  .peek {
    padding: 1px 4px;
    border-color: transparent;
    background: none;
    color: var(--activity-fg, var(--muted));
  }
  .more:hover,
  .peek:hover,
  .more:focus-visible,
  .peek:focus-visible {
    color: var(--fg);
    background: color-mix(in srgb, var(--fg) 4%, transparent);
    border-radius: 6px;
  }
  .peek :global(.chev) {
    opacity: 0.55;
  }
  .peek:hover :global(.chev) {
    opacity: 1;
  }
</style>
