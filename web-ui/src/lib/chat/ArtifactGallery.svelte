<script lang="ts">
  /**
   * What a turn wrote that its prose did not already show, after the
   * closing prose. The files its edit tools wrote (absolute paths from the
   * tools, so a chip opens whatever the prose called the file), plus the
   * files its shell commands wrote — the paths its commands and outputs
   * mention that the daemon confirms exist and were modified during the
   * turn (artifacts.ts). The reducer leaves out what the prose embedded or
   * linked, so this block adds and never repeats; its heading says "Also
   * written" when the prose showed a share. "Written", not "made": a turn
   * that edits a document wrote it too.
   *
   * One quiet line: the heading, then one chip per file (FileChip) —
   * figures, reports and clips alike with documents. A click opens the
   * file; resting the pointer on a chip previews that one file (the chat's
   * hover preview, `hoverTargets`: a picture, a PDF page, a document's
   * opening). Nothing draws inline — a turn that saves twenty plots still
   * costs a line or two, and the prose embeds the figures it talks about.
   * Past seven files, six show — one of each kind before a second of any
   * (`foldedChips`) — and the rest wait behind "+n more".
   *
   * A chip knows its file: two files sharing a name show their folders; a
   * file rewritten after the turn says so in its preview; a gone file is
   * struck and not clickable. States come from one resolve when the line
   * nears the viewport and follow the daemon's disk monitor while on screen
   * (the shown chips only — the watch list is small and shared).
   */
  import FileChip from "./FileChip.svelte";
  import { isMissing, resolveFile, type TargetInfo, type TargetResult } from "../shared/embed/embed";
  import type { OpenPathOptions, PathKind } from "../shared/openPath";
  import { lastDiskChange, releaseDiskFile, retainDiskFile } from "../workspace/diskWatch";
  import { artifactShape, chipLabels, fileStateAfter, foldedChips, writtenDuring, type FileState } from "./artifacts";
  import type { EmbedResolver } from "./embeds";
  import type { HoverTargets } from "./hoverTargets";

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
    /** The chat's hover registry: each known chip previews its file. */
    hoverTargets?: HoverTargets;
  }

  let {
    paths,
    mentioned = [],
    startedAtMs = null,
    endedAtMs = null,
    covered = [],
    resolver,
    onOpenPath,
    hoverTargets,
  }: Props = $props();

  /** Precise about what the block is: everything the turn wrote, or what
   *  is left once the prose has shown its share. */
  const heading = $derived(covered.length > 0 ? "Also written" : "Written this turn");

  /** Files listed at most: past this the turn wrote a directory's worth. */
  const MAX_FILES = 48;
  /** Chips on the line before the rest fold behind "+n more". */
  const MAX_CHIPS = 6;

  let host = $state<HTMLElement | null>(null);
  let near = $state(false);
  let onScreen = $state(false);
  /** Mentioned files confirmed written during the turn. */
  let confirmed = $state.raw<TargetInfo[]>([]);
  /** Each document's state now, by path; absent = not yet known. */
  let states = $state.raw<Record<string, FileState>>({});
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

  /** The mentioned files written during the turn, from their answers. */
  function confirm(answers: (TargetResult | null)[], start: number, end: number | null): TargetInfo[] {
    const known = new Set(paths);
    const out: TargetInfo[] = [];
    for (const a of answers) {
      if (a === null || isMissing(a) || a.kind !== "file" || known.has(a.path)) continue;
      if (!writtenDuring(a.mtime_ms, start, end)) continue;
      known.add(a.path);
      out.push(a);
    }
    return out;
  }

  // A gallery remounted by transcript paging rebuilds its tiles from the
  // resolver's recent answers before the first paint, so its height is
  // final at once instead of growing a round trip later under the reader.
  // svelte-ignore state_referenced_locally
  if (resolver !== undefined && startedAtMs !== null && mentioned.length > 0) {
    // svelte-ignore state_referenced_locally
    const answers = mentioned.map((m) => resolver.peek(m));
    // svelte-ignore state_referenced_locally
    if (answers.every((a) => a !== null)) confirmed = confirm(answers, startedAtMs, endedAtMs);
  }

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
      confirmed = confirm(answers, start, end);
    });
    return () => {
      stale = true;
    };
  });

  type Written = { path: string; info: TargetInfo | null };

  /** Every file, in the order the turn reported it: the tools' files, then
   *  the shell's once confirmed — so the line only grows at its end when
   *  the confirmations land. */
  const files = $derived.by((): Written[] => {
    const all: Written[] = paths.map((p) => ({ path: p, info: null }));
    for (const c of confirmed) all.push({ path: c.path, info: c });
    return all.filter((f) => artifactShape(f.path) !== null).slice(0, MAX_FILES);
  });
  /** Folding is only worth it past one extra chip's worth: "+1 more"
   *  costs the room the chip would. */
  const folds = $derived(files.length > MAX_CHIPS + 1);
  const chips = $derived(allChips || !folds ? files : foldedChips(files, MAX_CHIPS));
  /** Names widen against everything the turn wrote, shown here or not:
   *  `docs/notes.md` must not read "notes.md" beside the prose's link to
   *  the root one. */
  const labels = $derived(chipLabels([...files.map((f) => f.path), ...covered]));

  function setState(path: string, state: FileState): void {
    if (states[path] === state) return;
    states = { ...states, [path]: state };
  }

  // Each file's state, asked once when the line is near: a confirmed
  // mention was resolved just now (present by construction); a tool-written
  // path gets one coalesced fs/resolve_targets. `asked` is plain, not
  // reactive — reading `states` here would make this effect its own trigger.
  const asked = new Set<string>();
  $effect(() => {
    if (!near) return;
    const end = endedAtMs;
    for (const f of files) {
      if (asked.has(f.path)) continue;
      asked.add(f.path);
      if (f.info !== null) {
        setState(f.path, fileStateAfter(f.info, end));
        continue;
      }
      // Fresh: whether it changed after the turn is the question.
      void resolveFile(f.path, { fresh: true }).then((r) => {
        if (r !== null) setState(f.path, fileStateAfter(r, end));
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
          void resolveFile(t, { fresh: true }).then((r) => {
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

  /** A chip's accessible name: what a sighted reader gets from the chip
   *  and its preview. */
  function chipName(path: string, state: FileState): string {
    if (state === "gone") return `${path} · gone`;
    const verb = onOpenPath !== undefined ? "open " : "";
    return state === "changed" ? `${verb}${path} · changed after this turn` : `${verb}${path}`;
  }
</script>

<div class="gallery-host" bind:this={host}>
  {#if files.length > 0}
    <!-- One quiet line, the voice of a folded activity line: a chip per
         file, a rest on one previews it. -->
    <div class="files" role="group" aria-label={heading.toLowerCase()}>
      <span class="files-label">{heading}</span>
      {#each chips as f (f.path)}
        {@const state = states[f.path] ?? "present"}
        <FileChip
          path={f.path}
          label={labels.get(f.path) ?? f.path}
          {state}
          name={chipName(f.path, state)}
          title={state === "gone" ? chipName(f.path, state) : undefined}
          onOpen={onOpenPath !== undefined ? (e) => onOpenPath(f.path, "file", { split: e.metaKey || e.ctrlKey }) : undefined}
          hover={hoverTargets !== undefined && state !== "gone"
            ? {
                targets: hoverTargets,
                target: { path: f.path, fragment: null, ...(state === "changed" ? { note: "changed after this turn" } : {}) },
              }
            : null}
        />
      {/each}
      {#if folds}
        <!-- One button either way, so a keyboard toggle keeps its focus. -->
        <button class="more" aria-expanded={allChips} onclick={() => (allChips = !allChips)}>
          {allChips ? "fewer" : `+${files.length - chips.length} more`}
        </button>
      {/if}
    </div>
  {/if}
</div>

<style>
  .gallery-host {
    min-height: 1px;
    margin-top: 6px;
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
  /* "+n more" / "fewer": text-only, no chip edge. */
  .more {
    display: inline-flex;
    align-items: center;
    padding: 1px 4px;
    border: 1px solid transparent;
    border-radius: 6px;
    background: none;
    color: var(--activity-fg, var(--muted));
    font: inherit;
    font-size: var(--text-xs);
    line-height: 1.5;
    cursor: pointer;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }
  .more:hover,
  .more:focus-visible {
    color: var(--fg);
    background: color-mix(in srgb, var(--fg) 4%, transparent);
  }
</style>
