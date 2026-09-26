<script lang="ts">
  /**
   * Video and audio through the webview's native player. Bytes come from the
   * ticketed /raw/ URL — the daemon streams single byte ranges, so seeking
   * fetches only what it needs and nothing is buffered daemon-side — and the
   * bearer token never lands in a media URL.
   *
   * A ticket lives ~10 minutes, but a player keeps issuing range requests for
   * as long as it plays: when a load fails on an aged ticket, the store
   * re-mints it and the player resumes where it was (same for a re-mint
   * after the file changes on disk). A failure on a fresh ticket is the
   * format itself — a container is no promise of a codec (HEVC in a .mov,
   * Vorbis in WebKit) — and gets an honest card instead of a dead player,
   * with a download on a remote host so it can play locally.
   */
  import { untrack } from "svelte";
  import { basename, fsDownload, fsRawUrl, humanSize } from "./files";
  import { retain, release, type FileEntry } from "./fileStore.svelte";
  import { isRemoteHost } from "../net/api";
  import { activeSelection, clearSelection, setSelection, type FileSelection } from "../shared/reference";
  import { clockLabel, timeFragment } from "../shared/locator";
  import { revealRequest, takeReveal } from "../shared/reveal";
  import ReferenceButton from "../shared/ReferenceButton.svelte";
  import Spinner from "./Spinner.svelte";

  interface Props {
    path: string;
    kind: "video" | "audio";
  }

  let { path, kind }: Props = $props();

  let entry = $state<FileEntry | null>(null);
  $effect(() => {
    const e = retain(path);
    entry = e;
    void e.ensureRawUrl();
    return () => release(path);
  });

  const ticketError = $derived(entry?.rawError ?? null);
  let media = $state<HTMLMediaElement | null>(null);
  /** The URL the player holds; follows the store's, resuming the playhead. */
  let src = $state<string | null>(null);
  /** When `src`'s ticket was minted, to tell an aged ticket from a bad file. */
  let srcAt = 0;
  let resumeAt: { time: number; playing: boolean } | null = null;
  let failure = $state<string | null>(null);
  /** One re-mint per failure streak; a successful load clears it. */
  let retried = false;
  let duration = $state<number | null>(null);
  let dims = $state<{ w: number; h: number } | null>(null);
  let size = $state<number | null>(null);

  // Only a NEW store URL (first mint, a disk change) swaps the source; the
  // player and the current source are read untracked.
  $effect(() => {
    const e = entry;
    const u = e?.rawUrl ?? null;
    if (e === null || u === null) return;
    untrack(() => adopt(u, e.rawMintedAt));
  });

  function adopt(url: string, mintedAt: number): void {
    if (url === src) return;
    const el = media;
    if (el !== null && src !== null && el.currentTime > 0) {
      resumeAt = { time: el.currentTime, playing: !el.paused };
    }
    failure = null;
    src = url;
    srcAt = mintedAt;
  }

  function onMeta(): void {
    const el = media;
    if (el === null) return;
    duration = Number.isFinite(el.duration) ? el.duration : null;
    if (el instanceof HTMLVideoElement && el.videoWidth > 0) {
      dims = { w: el.videoWidth, h: el.videoHeight };
    }
    const r = resumeAt;
    resumeAt = null;
    if (r !== null) {
      el.currentTime = Math.min(r.time, el.duration || r.time);
      if (r.playing) void el.play().catch(() => {});
    }
    applyTime();
  }

  // --- pointing at a moment (context bridge) and landing on one (`#t=`) ------------

  /** The playhead, for the "reference this moment" button's label. */
  let now = $state(0);
  /** "mark range" was pressed once: the range starts here. */
  let markFrom = $state<number | null>(null);
  /** A marked (or revealed) range, in seconds. */
  let range = $state<{ start: number; end: number } | null>(null);
  /** A revealed range plays to its end, then pauses (once). */
  let stopAt: number | null = null;
  let pendingTime: { start: number; end?: number } | null = null;
  const selOwner = {};
  let published: FileSelection | null = null;

  function onTime(): void {
    const el = media;
    if (el === null) return;
    // Audio plays on in a hidden window: its label need not re-render there
    // (the next tick after the window returns catches it up).
    if (!document.hidden) now = el.currentTime;
    if (stopAt !== null && el.currentTime >= stopAt) {
      stopAt = null;
      el.pause();
    }
  }

  function momentSelection(): FileSelection {
    const r = range;
    // The live playhead at click time (the label may trail it by a tick).
    const t = media?.currentTime ?? now;
    const fragment = r !== null ? timeFragment(r.start, r.end) : timeFragment(t);
    const label = r !== null ? `${clockLabel(r.start)}–${clockLabel(r.end)}` : clockLabel(t);
    return { kind: "file", path, startLine: null, endLine: null, text: "", fragment, label };
  }

  const momentLabel = $derived(
    range !== null ? `${clockLabel(range.start)}–${clockLabel(range.end)}` : clockLabel(now),
  );

  /** First press marks the start, the second the end (either order). */
  function mark(): void {
    const t = media?.currentTime ?? now;
    if (markFrom === null) {
      markFrom = t;
      return;
    }
    const a = Math.min(markFrom, t);
    const b = Math.max(markFrom, t);
    markFrom = null;
    if (b - a < 0.05) return;
    range = { start: a, end: b };
    // A marked range is a selection: the reference chord sends it too.
    const sel = momentSelection();
    published = sel;
    setSelection(selOwner, sel);
  }

  function clearRange(): void {
    range = null;
    markFrom = null;
    stopAt = null;
    if (published !== null) {
      published = null;
      clearSelection(selOwner);
    }
  }

  $effect(() => {
    const a = $activeSelection;
    if (published !== null && a !== published) published = null;
  });

  $effect(() => () => {
    if (published !== null) clearSelection(selOwner);
    published = null;
  });

  $effect(() => {
    void $revealRequest;
    const req = takeReveal(path);
    if (req?.time === undefined) return;
    pendingTime = req.time;
    untrack(applyTime);
  });

  /** Seek to a revealed moment once the player knows its duration. */
  function applyTime(): void {
    const el = media;
    const t = pendingTime;
    if (el === null || t === null || el.readyState < HTMLMediaElement.HAVE_METADATA) return;
    pendingTime = null;
    const end = Number.isFinite(el.duration) ? el.duration : Infinity;
    el.currentTime = Math.min(t.start, end);
    now = el.currentTime;
    if (t.end !== undefined && t.end > t.start) {
      range = { start: t.start, end: Math.min(t.end, end) };
      stopAt = range.end;
    }
  }

  function onLoaded(): void {
    retried = false;
    failure = null;
  }

  /** Past this a ticket may be the problem rather than the file. */
  const AGED_TICKET_MS = 60_000;

  async function onError(): Promise<void> {
    const el = media;
    const e = entry;
    if (el === null || e === null || src === null) return;
    const code = el.error?.code ?? 0;
    // A 404 from an expired ticket surfaces as "source not supported" in
    // Chromium and as a network error in WebKit.
    const maybeTicket =
      code === MediaError.MEDIA_ERR_NETWORK || code === MediaError.MEDIA_ERR_SRC_NOT_SUPPORTED;
    if (maybeTicket && !retried && Date.now() - srcAt > AGED_TICKET_MS) {
      retried = true;
      const resume = { time: el.currentTime, playing: !el.paused };
      try {
        const fresh = await fsRawUrl(path);
        if (entry !== e) return;
        adopt(fresh, Date.now());
        resumeAt = resume;
        return;
      } catch {
        // unreachable daemon: report the player's own failure below
      }
    }
    failure = describe(code);
    probeSize();
  }

  function describe(code: number): string {
    switch (code) {
      case MediaError.MEDIA_ERR_DECODE:
        return `this ${kind}'s codec can't be decoded here`;
      case MediaError.MEDIA_ERR_NETWORK:
        return `the ${kind} stopped loading`;
      case MediaError.MEDIA_ERR_ABORTED:
        return "loading was interrupted";
      default:
        return `this ${kind} format can't be played here`;
    }
  }

  /** The card shows the size; a failed player never learned it. */
  function probeSize(): void {
    const url = src;
    if (url === null || size !== null) return;
    void fetch(url, { method: "HEAD" })
      .then((r) => {
        const n = Number(r.headers.get("Content-Length"));
        if (r.ok && Number.isFinite(n)) size = n;
      })
      .catch(() => {});
  }

  /** A fresh ticket (the old one may be the failure) and a reload. */
  async function retry(): Promise<void> {
    const e = entry;
    failure = null;
    retried = false;
    try {
      const fresh = await fsRawUrl(path);
      if (entry === e) adopt(fresh, Date.now());
    } catch {
      failure = "the daemon couldn't be reached";
    }
  }

  // A parked pane keeps this view alive but unseen: a video pauses rather
  // than decode frames nobody sees (it does not resume by itself). Audio
  // keeps playing — listening while working elsewhere is the point.
  $effect(() => {
    const el = media;
    if (el === null || kind !== "video") return;
    const layer = el.closest<HTMLElement>(".layer");
    if (layer === null) return;
    const watch = new MutationObserver(() => {
      if (layer.inert && !el.paused) el.pause();
    });
    watch.observe(layer, { attributes: true, attributeFilter: ["inert"] });
    return () => watch.disconnect();
  });

  // Dropping the element alone can leave its range request open until GC;
  // an explicit unload closes it now.
  $effect(() => {
    const el = media;
    if (el === null) return;
    return () => {
      el.pause();
      el.removeAttribute("src");
      el.load();
    };
  });

  function fmtTime(s: number): string {
    const t = Math.round(s);
    const h = Math.floor(t / 3600);
    const m = Math.floor((t % 3600) / 60);
    const sec = String(t % 60).padStart(2, "0");
    return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${sec}` : `${m}:${sec}`;
  }

  const facts = $derived(
    [dims === null ? null : `${dims.w}×${dims.h}`, duration === null ? null : fmtTime(duration)]
      .filter((f) => f !== null)
      .join(" · "),
  );

  const remote = isRemoteHost();
</script>

<div class="media-view">
  <div class="media-bar">
    <span class="facts" class:dim={facts === ""}>{facts === "" ? kind : facts}</span>
    <span class="spacer"></span>
    {#if src !== null && failure === null}
      {#if range !== null}
        <!-- The @ button carries the range; this only lets go of it. -->
        <button class="bbtn" onclick={clearRange} title="clear the range and point at the playhead again"
          >clear range</button
        >
      {:else}
        <button
          class="bbtn"
          class:on={markFrom !== null}
          onclick={mark}
          title={markFrom === null
            ? "mark the start of a range to reference (press again at its end)"
            : "mark the end of the range here"}
          >{markFrom === null ? "mark range" : `from ${clockLabel(markFrom)} · mark end`}</button
        >
      {/if}
      <ReferenceButton pick={momentSelection} label={momentLabel} text={momentLabel} />
    {/if}
    {#if remote}
      <button class="bbtn" onclick={() => void fsDownload(path)} title="download to this computer"
        >download</button
      >
    {/if}
  </div>

  <div class="stage" class:audio={kind === "audio"}>
    {#if ticketError !== null}
      <div class="card"><span class="note">{ticketError}</span></div>
    {:else if failure !== null}
      <div class="card" role="status">
        <svg viewBox="0 0 24 24" width="26" height="26" aria-hidden="true">
          {#if kind === "video"}
            <rect x="3" y="5" width="13" height="14" rx="2" fill="none" stroke="currentColor" stroke-width="1.4" />
            <path d="M16 10l5-3v10l-5-3" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" />
          {:else}
            <path d="M9 18V6l11-2v12" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" />
            <circle cx="6.5" cy="18" r="2.5" fill="none" stroke="currentColor" stroke-width="1.4" />
            <circle cx="17.5" cy="16" r="2.5" fill="none" stroke="currentColor" stroke-width="1.4" />
          {/if}
        </svg>
        <span class="name">{basename(path)}</span>
        {#if size !== null}<span class="size">{humanSize(size)}</span>{/if}
        <span class="note">{failure}</span>
        <div class="actions">
          <button class="opt" onclick={() => void retry()}>try again</button>
          {#if remote}
            <button class="opt primary" onclick={() => void fsDownload(path)}>download</button>
          {/if}
        </div>
        {#if !remote}<span class="hint">a desktop player can open it from its folder</span>{/if}
      </div>
    {:else if src !== null}
      {#if kind === "video"}
        <!-- A workspace file has no caption track to offer. -->
        <!-- svelte-ignore a11y_media_has_caption -->
        <video
          bind:this={media}
          {src}
          controls
          playsinline
          preload="metadata"
          aria-label={basename(path)}
          onloadedmetadata={onMeta}
          onloadeddata={onLoaded}
          ontimeupdate={onTime}
          onseeked={onTime}
          onerror={() => void onError()}
        ></video>
      {:else}
        <div class="player">
          <span class="name">{basename(path)}</span>
          <audio
            bind:this={media}
            {src}
            controls
            preload="metadata"
            aria-label={basename(path)}
            onloadedmetadata={onMeta}
            onloadeddata={onLoaded}
            ontimeupdate={onTime}
            onseeked={onTime}
            onerror={() => void onError()}
          ></audio>
        </div>
      {/if}
    {:else}
      <Spinner />
    {/if}
  </div>
</div>

<style>
  .media-view {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
  }

  .media-bar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.6rem;
    height: 26px;
    padding: 0 0.7rem;
    border-bottom: 1px solid var(--edge);
    font-size: var(--text-xs);
    color: var(--muted);
  }

  .facts {
    font-family: var(--mono);
    font-variant-numeric: tabular-nums;
  }

  .facts.dim {
    opacity: 0.6;
  }

  .spacer {
    flex: 1;
  }

  .bbtn {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: pointer;
    padding: 0.1rem 0.4rem;
    border-radius: 4px;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .bbtn:hover {
    background: var(--row-hover);
    color: var(--fg);
  }

  .bbtn.on {
    color: var(--accent);
  }



  .stage {
    position: relative;
    flex: 1;
    min-height: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 12px;
    background: color-mix(in srgb, var(--fg) 4%, var(--term-bg));
  }

  .stage.audio {
    background: var(--term-bg);
  }

  video {
    display: block;
    max-width: 100%;
    max-height: 100%;
    border-radius: 4px;
    /* Letterbox bars stay a video's own black whatever the theme. */
    background: #000;
    box-shadow: 0 1px 6px color-mix(in srgb, var(--fg) 18%, transparent);
  }

  .player {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.8rem;
    width: min(560px, 92%);
  }

  audio {
    width: 100%;
  }

  .card {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.5rem;
    color: var(--muted);
    max-width: 80%;
    text-align: center;
  }

  .card svg {
    opacity: 0.55;
  }

  .name {
    font-family: var(--mono);
    font-size: var(--text-md);
    color: var(--fg);
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .size {
    font-family: var(--mono);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
  }

  .note {
    font-size: var(--text-sm);
  }

  .actions {
    display: flex;
    gap: 0.5rem;
    margin-top: 0.3rem;
  }

  .hint {
    font-size: var(--text-xs);
    opacity: 0.75;
  }

  @media (prefers-reduced-motion: reduce) {
    .bbtn {
      transition: none;
    }
  }
</style>
