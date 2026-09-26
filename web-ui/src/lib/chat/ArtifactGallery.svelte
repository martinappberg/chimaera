<script lang="ts">
  /**
   * What a turn made, shown after its closing prose as embed cards (compact
   * tiles): the files its edit tools wrote (absolute paths from the tools,
   * so a tile opens whatever the prose called the file), plus the files its
   * shell commands wrote — the paths its commands, outputs and prose
   * mention that the daemon confirms exist and were modified during the
   * turn (artifacts.ts). Plots, HTML reports, documents, tables, notebooks,
   * slides, media. Tiles load near the viewport, stay fresh when a file is
   * overwritten, and say so when one is gone.
   */
  import EmbedCard from "../shared/embed/EmbedCard.svelte";
  import { isMissing, type TargetInfo, type TargetResult } from "../shared/embed/embed";
  import type { OpenPathOptions, PathKind } from "../shared/openPath";
  import { writtenDuring } from "./artifacts";
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

  /** Tiles per turn: past this the turn made a directory's worth. */
  const MAX_TILES = 8;

  let host = $state<HTMLElement | null>(null);
  let near = $state(false);
  /** Mentioned files confirmed written during the turn. */
  let confirmed = $state.raw<TargetInfo[]>([]);

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

  const tiles = $derived.by(() => {
    const out: { path: string; info: TargetInfo | null }[] = paths.map((p) => ({ path: p, info: null }));
    for (const c of confirmed) out.push({ path: c.path, info: c });
    return out.slice(0, MAX_TILES);
  });

  function open(path: string, kind: "file" | "dir", reveal?: import("../shared/reveal").Reveal): void {
    onOpenPath?.(path, kind, reveal !== undefined ? { reveal } : {});
  }
</script>

<div class="gallery-host" bind:this={host}>
  {#if tiles.length > 0}
    <div class="label">
      Made this turn · {tiles.length} file{tiles.length === 1 ? "" : "s"}
    </div>
    <div class="gallery" aria-label="files made this turn">
      {#each tiles as tile (tile.path)}
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
  {/if}
</div>

<style>
  .gallery-host {
    min-height: 1px;
  }
  .label {
    margin: 8px 0 4px;
    color: var(--activity-fg, var(--muted));
    font-size: 12px;
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
</style>
