<script lang="ts">
  /**
   * What a turn wrote that its prose did not already show, after the
   * closing prose. The files its edit tools wrote (absolute paths from the
   * tools, so a tile opens whatever the prose called the file), plus the
   * files its shell commands wrote — the paths its commands and outputs
   * mention that the daemon confirms exist and were modified during the
   * turn (artifacts.ts). The reducer leaves out what the prose embedded or
   * linked, so this block adds and never repeats; its heading says "Also
   * written" when the prose showed a share. "Written", not "made": a turn
   * that edits a document wrote it too.
   *
   * One header, two shapes by what a file is for (artifactShape): a
   * *visual* — a figure, a rendered report, a PDF, a clip — is looked at,
   * so it gets an embed tile; a *document* — a markdown note, a table, a
   * notebook, a deck — is opened, so it gets one chip on a quiet line, and
   * its tile only when the line is unfolded. With no tiles the header sits
   * on the chip line, so a turn that only touched documents costs one line.
   *
   * A chip knows its file: two files sharing a name show their folders; a
   * file rewritten after the turn says so in its tooltip; a gone file is
   * struck and not clickable. States come from one resolve when the gallery
   * nears the viewport and follow the daemon's disk monitor while on screen
   * (the shown chips only — the watch list is small and shared). Tiles load
   * near the viewport, stay fresh when a file is overwritten, and say so
   * when one is gone.
   */
  import Chevron from "../shared/Chevron.svelte";
  import FileIcon from "../shared/FileIcon.svelte";
  import EmbedCard from "../shared/embed/EmbedCard.svelte";
  import { isMissing, resolveFile, type TargetInfo, type TargetResult } from "../shared/embed/embed";
  import type { OpenPathOptions, PathKind } from "../shared/openPath";
  import { lastDiskChange, releaseDiskFile, retainDiskFile } from "../workspace/diskWatch";
  import { artifactShape, chipLabels, fileStateAfter, writtenDuring, type FileState } from "./artifacts";
  import type { EmbedResolver } from "./embeds";

  interface Props {
    /** Files the turn's tools reported writing (absolute). */
    paths: string[];
    /** Artifact-shaped paths the turn mentioned (as written). */
    mentioned?: string[];
    startedAtMs?: number | null;
    endedAtMs?: number | null;
    /** What the turn wrote that the prose already showed: this block is
     *  the remainder (it says "also"), and a chip's name widens against
     *  these too. */
    covered?: string[];
    /** Resolves mentioned paths against the session's directories. */
    resolver?: EmbedResolver;
    onOpenPath?: (path: string, kind: PathKind, opts?: OpenPathOptions) => void;
  }

  let {
    paths,
    mentioned = [],
    startedAtMs = null,
    endedAtMs = null,
    covered = [],
    resolver,
    onOpenPath,
  }: Props = $props();

  /** Precise about what the block is: everything the turn wrote, or what
   *  is left once the prose has shown its share. */
  const heading = $derived(covered.length > 0 ? "Also written" : "Written this turn");

  /** Tiles per shape: past this the turn wrote a directory's worth. */
  const MAX_TILES = 8;
  /** Chips on the line before the rest fold behind "+n more". */
  const MAX_CHIPS = 6;

  let host = $state<HTMLElement | null>(null);
  let near = $state(false);
  let onScreen = $state(false);
  /** Mentioned files confirmed written during the turn. */
  let confirmed = $state.raw<TargetInfo[]>([]);
  /** Each document's state now, by path; absent = not yet known. */
  let states = $state.raw<Record<string, FileState>>({});
  /** The documents' tiles, unfolded by the reader. */
  let peek = $state(false);
  let allChips = $state(false);

  $effect(() => {
    const el = host;
    if (el === null) return;
    if (typeof IntersectionObserver === "undefined") {
      near = true;
      onScreen = true;
      return;
    }
    const root = el.closest(".transcript");
    const nearing = new IntersectionObserver(
      (entries) => {
        if (!entries.some((e) => e.isIntersecting)) return;
        near = true;
        nearing.disconnect();
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
  const chips = $derived(allChips || documents.length <= MAX_CHIPS ? documents : documents.slice(0, MAX_CHIPS));
  /** The fold previews the first tiles' worth; the chips still name them all. */
  const previewed = $derived(documents.slice(0, MAX_TILES));
  /** Names widen against everything the turn wrote, shown here or not:
   *  `docs/notes.md` must not read "notes.md" beside the prose's link to
   *  the root one. */
  const labels = $derived(chipLabels([...documents.map((d) => d.path), ...covered]));

  function setState(path: string, state: FileState): void {
    if (states[path] === state) return;
    states = { ...states, [path]: state };
  }

  // Each document's state, asked once when the gallery is near: a confirmed
  // mention was resolved just now (present by construction); a tool-written
  // path gets one coalesced fs/resolve_targets. `asked` is plain, not
  // reactive — reading `states` here would make this effect its own trigger.
  const asked = new Set<string>();
  $effect(() => {
    if (!near) return;
    const end = endedAtMs;
    for (const doc of documents) {
      if (asked.has(doc.path)) continue;
      asked.add(doc.path);
      if (doc.info !== null) {
        setState(doc.path, fileStateAfter(doc.info, end));
        continue;
      }
      void resolveFile(doc.path).then((r) => {
        if (r !== null) setState(doc.path, fileStateAfter(r, end));
      });
    }
  });

  // While the line is on screen, the shown chips follow the disk monitor: a
  // delete strikes the chip, a rewrite re-resolves its mtime.
  const watched = $derived(chips.map((c) => c.path));
  $effect(() => {
    const targets = watched;
    const end = endedAtMs;
    if (!onScreen || targets.length === 0) return;
    for (const t of targets) retainDiskFile(t);
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
      for (const t of targets) {
        if (change.removed.includes(t)) setState(t, "gone");
        else if (change.files.includes(t)) {
          void resolveFile(t).then((r) => {
            if (r !== null) setState(t, fileStateAfter(r, end));
          });
        }
      }
    });
    return () => {
      stop();
      for (const t of targets) releaseDiskFile(t);
    };
  });

  function open(path: string, kind: "file" | "dir", reveal?: import("../shared/reveal").Reveal): void {
    onOpenPath?.(path, kind, reveal !== undefined ? { reveal } : {});
  }

  function chipTitle(path: string, state: FileState): string {
    if (state === "gone") return `${path} · gone`;
    const verb = onOpenPath !== undefined ? "open " : "";
    return state === "changed" ? `${verb}${path} · changed after this turn` : `${verb}${path}`;
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
    <div class="label">{heading}</div>
    {@render tiles(visuals, heading.toLowerCase())}
  {/if}
  {#if documents.length > 0}
    <!-- Documents are opened, not stared at: one chip each, the same quiet
         voice as a folded activity line, and their tiles behind a fold. -->
    <div class="files" role="group" aria-label="documents written this turn">
      {#if visuals.length === 0}
        <span class="files-label">{heading}</span>
      {/if}
      {#each chips as doc (doc.path)}
        {@const state = states[doc.path] ?? "present"}
        <button
          class="chip"
          class:gone={state === "gone"}
          title={chipTitle(doc.path, state)}
          disabled={onOpenPath === undefined || state === "gone"}
          onclick={() => open(doc.path, "file")}
        >
          <FileIcon path={doc.path} size={13} broken={state === "gone"} />
          <span class="name">{labels.get(doc.path) ?? doc.path}</span>
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
      {@render tiles(previewed, "documents written this turn, previewed")}
    {/if}
  {/if}
</div>

<style>
  .gallery-host {
    min-height: 1px;
    margin-top: 6px;
  }
  .label {
    margin: 8px 0 4px;
    color: var(--activity-fg, var(--muted));
    font-size: var(--text-xs);
  }
  /* A figure strip, not a grid of boxes: tiles share one row height and
     take their width from what they show — a picture's own aspect ratio, a
     report's page width — and wrap like figures laid on a desk. */
  .gallery {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-start;
    gap: 10px;
    margin: 0 0 10px;
  }
  .tile {
    display: flex;
    flex: none;
    width: fit-content;
    min-width: 200px;
    max-width: 100%;
    height: 230px;
    border-radius: 10px;
  }
  /* The shared embed card, worn as a tile: the name becomes a caption
     under the picture, the picture fills the tile to its edges, and the
     tile's width follows the picture. */
  .tile > :global(.embed-card) {
    flex: 1;
    flex-direction: column-reverse;
    width: fit-content;
    min-width: 100%;
    border-radius: 10px;
  }
  .tile > :global(.embed-card > .head) {
    border-bottom: none;
    border-top: 1px solid color-mix(in srgb, var(--edge) 55%, transparent);
  }
  .tile :global(.image-body.tile) {
    padding: 0;
    background: none;
  }
  .tile :global(.image-body.tile .frame) {
    border-radius: 0;
  }
  /* A picture's tile hugs the picture (drawn at its own size, never
     scaled up, at most a row tall): a wide figure is a wide, short tile,
     not a figure floating in a box. */
  .tile:has(> :global(.embed-card[data-embed-kind="image"])) {
    height: auto;
    max-height: 230px;
  }
  .tile > :global(.embed-card[data-embed-kind="image"]) {
    height: auto;
  }
  /* Kinds without a natural width take a page's worth. */
  .tile > :global(.embed-card[data-embed-kind="html"]),
  .tile > :global(.embed-card[data-embed-kind="video"]),
  .tile > :global(.embed-card[data-embed-kind="audio"]) {
    width: 320px;
  }
  .tile > :global(.embed-card[data-embed-kind="pdf"]) {
    width: 220px;
  }
  .tile > :global(.embed-card[data-embed-kind="markdown"]),
  .tile > :global(.embed-card[data-embed-kind="table"]),
  .tile > :global(.embed-card[data-embed-kind="xlsx"]),
  .tile > :global(.embed-card[data-embed-kind="notebook"]),
  .tile > :global(.embed-card[data-embed-kind="code"]),
  .tile > :global(.embed-card[data-embed-kind="file"]),
  .tile > :global(.embed-card[data-embed-kind="pending"]) {
    width: 280px;
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
  /* Gone: still named (the turn did write it), plainly not there any more. */
  .chip.gone {
    color: var(--muted);
    border-style: dashed;
    background: none;
  }
  .chip.gone .name {
    text-decoration: line-through;
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
