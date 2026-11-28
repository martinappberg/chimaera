<script lang="ts">
  /**
   * A PowerPoint deck (.pptx) drawn in the browser by pptx-renderer, in the
   * slides chrome the Marp viewer uses: one slide on the stage, a thumbnail
   * strip, arrow-key paging, a full-screen present mode, `#slide=N` reveals,
   * and the speaker notes under the stage when the deck has any.
   *
   * Slides render lazily: the stage keeps the current slide and its
   * neighbours, and the strip mounts only the thumbnails near its visible
   * span. The package is loaded with the renderer's zip limits for untrusted
   * input and every external load target stripped first (`pptxDeck.ts`), so
   * a deck never fetches anything; web links in it open through the app.
   * Builds play click by click in the show, with no motion yet: each click
   * lands on the state PowerPoint's show reaches after it (`pptxTiming.ts`
   * parses the slide's timing, `pptxTag.ts` finds its shapes in the drawn
   * slide, `pptxPlay.ts` hides and offsets them). Browsing shows each slide
   * as drawn until a click dot in the bar is used; thumbnails always do.
   *
   * Context bridge: the slide's @ sends the slide (its title and text); the
   * area tool (or Shift-drag) sends a box on it in points, a plain click in
   * the tool the shape under the pointer, with the shapes' names and text
   * (`pptxRefs.ts`). `#slide=N&xywh=…` reveals that slide with the box
   * outlined.
   *
   * Edit mode (the bar's edit, `PptxEditLayer.svelte`): move, resize and
   * delete shapes, edit their text, drag thumbnails to reorder slides. The
   * edits live in `pptxEditStore.svelte.ts` (undo, unsaved state, journal,
   * the hash-checked save that rewrites only the touched parts); a slide an
   * edit touched is redrawn from its patched XML, so the stage and strip
   * always show the renderer's real output. "PowerPoint" opens the deck in
   * the real app through the native shell (a round trip for a remote host).
   *
   * The show, the presenter's view and paging run on the show controller
   * (`show/showController.svelte.ts`): **presenter** puts the stage (this slide at its build) beside the
   * next click, drawn live by a second render of that slide with its state
   * on it, over the speaker notes, a timer and the strip of every slide;
   * **show in window** opens this tab again in a new window that starts its
   * own show and follows this one (and this one it) by slide part and build.
   * A click in the show advances, except on a link the slide made
   * navigable or while text is being selected.
   */
  import { tick, untrack } from "svelte";
  import type { SlideData, SlideHandle } from "@aiden0z/pptx-renderer";
  import { materializeSlideNodes, renderSlide } from "@aiden0z/pptx-renderer";
  import { basename, dirname, fsDownload, fsList } from "./files";
  import { onDiskVersion, ownedObjectUrls, paneVisibility, useFileEntry } from "./viewLifecycle.svelte";
  import { fetchRawBytes, TooLargeError } from "./rawBytes";
  import { loadDeck, loadNotes, type Deck } from "./pptxDeck";
  import { showPosition, showReach } from "./slideShow";
  import { SlideShow } from "./show/showController.svelte";
  import { syncName } from "./show/showSync";
  import PresenterView from "./show/PresenterView.svelte";
  import { copyTabToWindow } from "../plugins/platform";
  import { buildStates, extractTiming, parseTimeline, type BuildState, type SlideTimeline, type TimingExtract } from "./pptxTiming";
  import { pruneTagged, tagSlide, type TaggedSlide } from "./pptxTag";
  import { applyState, clearState } from "./pptxPlay";
  import { sanitizeRendered } from "./officeSafety";
  import { revealRequest, takeReveal } from "../shared/reveal";
  import { activateUrl } from "../shared/urlOpen";
  import { getActiveWorkspaceId, isRemoteHost } from "../net/api";
  import Spinner from "./Spinner.svelte";
  import ReferenceButton from "../shared/ReferenceButton.svelte";
  import ReferenceChip from "../shared/ReferenceChip.svelte";
  import { activeSelection, clearSelection, setSelection, type FileSelection } from "../shared/reference";
  import { dragRegion, screenToPage, type Point, type Region } from "./imageRegion";
  import { areaReference, itemAt, PX_TO_PT, slideItems, slideReference, snapBox, type SlideItem } from "./pptxRefs";
  import PptxEditLayer from "./PptxEditLayer.svelte";
  import { deckEdits } from "./pptxEditStore.svelte";
  import { sha256Hex } from "./pptxPackage";
  import { slideOrder } from "./pptxXml";
  import {
    canOpenInOffice,
    listenOfficeTrip,
    officeTrip,
    openInOffice,
    resolveOfficeTrip,
    type OfficeTrip,
  } from "../net/native";

  interface Props {
    path: string;
    /** The pane shows this tab (Pane, via FileView); unset: watch the layer. */
    visible?: boolean;
  }

  let { path, visible = undefined }: Props = $props();

  const MAX_PPTX_BYTES = 100 * 1024 * 1024;
  const THUMB_H = 62;
  const THUMB_GAP = 8;
  /** Rendered stage slides kept for instant paging back and forth. */
  const STAGE_KEEP = 4;
  /** Thumbnails mounted beyond the strip's visible span, per side. */
  const THUMB_OVERSCAN = 6;

  const file = useFileEntry(
    () => path,
    (e) => void e.ensureMtime(),
  );

  let deck = $state.raw<Deck | null>(null);
  let error = $state<string | null>(null);
  let tooLarge = $state(false);
  let showThumbs = $state(true);
  let showNotes = $state(true);
  let hasNotes = $state(false);
  let nodeErrors = $state(0);
  let pendingSlide: number | null = null;
  /** Bumped when a new deck lands, so the mount effects start over. */
  let deckGen = $state(0);
  /** The view's root: what a show window copies (its pane's tab). */
  let viewEl = $state<HTMLDivElement | null>(null);

  /** Two windows on this file keep step on this channel. */
  const wsId = getActiveWorkspaceId() ?? "";
  const syncKey = $derived(syncName(wsId, "file", path));

  const show = new SlideShow({
    deck: {
      count: () => deck?.presentation.slides.length ?? 0,
      hidden: () => hiddenFlags,
      builds: (need) => {
        for (const j of need) timelineOf(j);
        return buildCounts();
      },
      // The slide part: a reorder or an edit elsewhere doesn't change it.
      key: (i) => deck?.presentation.slides[i]?.slidePath ?? String(i),
      find: (key) => {
        const slides = deck?.presentation.slides ?? [];
        const i = slides.findIndex((sl) => sl.slidePath === key);
        if (i >= 0) return i;
        const n = /^[0-9]{1,6}$/.test(key) ? Number(key) : -1;
        // A slide with no part path is named by its position.
        return n >= 0 && n < slides.length ? n : null;
      },
    },
    sync: () => syncKey,
    visible: () => onScreen.shown,
    showEl: () => stageEl,
    openWindow: () => (viewEl === null ? Promise.resolve(false) : copyTabToWindow(viewEl)),
    browseBySlide: true,
    endOnLastBuild: false,
    // The show opens a slide before its first click, as PowerPoint's does;
    // the presenter's view keeps a build a click dot chose.
    opening: (mode, at) => ({ slide: at.slide, build: mode === "presenter" && !asDrawn ? at.build : 0 }),
    arrive: (from, to) => {
      if (to.slide === from.slide) return;
      clearPick();
      if (show.mode === "browse") asDrawn = true;
    },
    left: () => (asDrawn = true),
  });
  const current = $derived(show.at.slide);
  /** The show has the screen (or covers the window). */
  const presenting = $derived(show.mode === "present");
  /** The show or the presenter's view: builds step, hidden slides skip. */
  const inShow = $derived(show.mode !== "browse");

  /** Blob URLs for the deck's media, shared by every slide render; revoked
   *  when the next deck lands and when the view goes. */
  const media = ownedObjectUrls();
  let mediaUrls = media.cache();

  function revokeMedia(): void {
    media.revokeAll();
    mediaUrls = media.cache();
  }

  async function readDeck(signal: AbortSignal): Promise<{ d: Deck; bytes: ArrayBuffer }> {
    const bytes = await fetchRawBytes(path, MAX_PPTX_BYTES, signal);
    let d: Deck;
    try {
      d = await loadDeck(bytes);
    } catch (e) {
      const msg = e instanceof Error ? e.message : "";
      throw new Error(
        /limit exceeded/i.test(msg)
          ? `this deck is past the viewer's safety limits (${msg.replace(/^.*limit exceeded:\s*/i, "")})`
          : "this file couldn't be read as a PowerPoint deck (legacy .ppt files aren't supported)",
      );
    }
    if (d.presentation.slides.length === 0) throw new Error("this deck has no slides");
    return { d, bytes };
  }

  // Open, and re-read on every on-disk version. A version that lands while
  // the tab is parked is parsed off screen and drawn by the reveal.
  const onScreen = paneVisibility(
    () => visible,
    () => slot,
  );
  onDiskVersion(() => file.current, () => onScreen.pane, {
    // This window's own save: the stage already shows exactly what it wrote
    // (the edited slides were redrawn from it), so there's nothing to read —
    // re-reading would refetch the whole deck and redraw every slide.
    load: async (signal) => {
      const m = untrack(() => file.current?.mtime ?? null);
      if (untrack(() => deck !== null && edits.ready) && file.current?.isOwnWrite(m) === true) return null;
      return readDeck(signal);
    },
    apply: (got, still) => {
      if (got === null) return;
      // Mid-typing or mid-drag the drawn slide is being edited: take the new
      // version when that ends (the history rebases onto it then).
      if (holding) {
        heldVersion = { got, still };
        return;
      }
      applyVersion(got, still);
    },
    fail: (e) => {
      tooLarge = e instanceof TooLargeError;
      error = e instanceof Error ? e.message : "the deck could not be opened";
    },
    whileHidden: "defer",
  });

  /** A disk version that landed while the slide was held. */
  let heldVersion: { got: { d: Deck; bytes: ArrayBuffer }; still: () => boolean } | null = null;
  $effect(() => {
    if (holding) return;
    const h = heldVersion;
    heldVersion = null;
    if (h !== null && h.still()) untrack(() => applyVersion(h.got, h.still));
  });

  function applyVersion({ d, bytes }: { d: Deck; bytes: ArrayBuffer }, still: () => boolean): void {
      disposeAll();
      revokeMedia();
      clearPick();
      lastBytes = bytes;
      drawn.clear();
      resetTimelines(d);
      relTargets = null;
      deck = d;
      deckGen += 1;
      error = null;
      tooLarge = false;
      nodeErrors = 0;
      hasNotes = false;
      // A re-read mid-show stays on the same slide part (both windows do),
      // the build held to the slide's clicks.
      const want = pendingSlide;
      pendingSlide = null;
      if (want !== null) go(want);
      else show.relocate();
      void loadNotes(bytes, d).then(
        (any) => {
          if (still()) hasNotes = any;
        },
        () => {},
      );
      // A history (this view's, or one a closed view left unsaved) follows
      // every version on disk: our own save is recognized, anything else is
      // rebased, and the touched slides are drawn edited. Unsaved edits a
      // crash or reload left in the journal come back at once, not at the
      // next press of edit (the unsaved dot and the note show they're there).
      if (edits.ready || editMode) void attachEdits(bytes, still);
      else
        void edits.hasJournal().then((has) => {
          if (has && still() && !disposed) void attachEdits(bytes, still);
        });
  }

  $effect(() => () => disposeAll());

  const count = $derived(deck?.presentation.slides.length ?? 0);
  const w = $derived(deck?.presentation.width ?? 960);
  const h = $derived(deck?.presentation.height ?? 540);

  // --- rendering ------------------------------------------------------------------

  /** Draw slide `i`. `tag`: a stage slide, whose shapes builds act on. */
  function render(i: number, tag = false): SlideHandle | null {
    const d = deck;
    if (d === null) return null;
    const slide = d.presentation.slides[i];
    if (slide === undefined) return null;
    let handle: SlideHandle;
    try {
      handle = renderSlide(d.presentation, slide, {
        mediaUrlCache: mediaUrls,
        pdfjs: false,
        onNodeError: () => (nodeErrors += 1),
        onNavigate: (t) => {
          navigatedAt = performance.now();
          if (t.slideIndex !== undefined) go(t.slideIndex);
          else if (t.url !== undefined) activateUrl(t.url);
        },
      });
    } catch {
      nodeErrors += 1;
      return null;
    }
    // A slide that fails after it was drawn (tagging, sanitizing) is let go
    // whole: an unsanitized slide must never reach the page.
    try {
      // Tagged before sanitizing, which keeps `data-*` but may drop elements.
      const tagged = tag ? tagSlide(handle.element, slide.nodes) : null;
      // Defense in depth: the package was stripped of remote targets, and
      // this makes sure nothing in the laid-out slide still points out.
      sanitizeRendered([handle.element]);
      if (tagged !== null) {
        pruneTagged(handle.element, tagged);
        stageTags.set(handle.element, tagged);
      }
      return handle;
    } catch {
      handle.dispose();
      handle.element.remove();
      nodeErrors += 1;
      return null;
    }
  }

  /** Stage handles by slide index, most recently used last. */
  const stageHandles = new Map<number, SlideHandle>();
  const thumbHandles = new Map<number, SlideHandle>();
  /** Each stage slide's shapes by id, tagged as it was drawn. */
  const stageTags = new WeakMap<HTMLElement, TaggedSlide>();

  function disposeAll(): void {
    for (const h of [...stageHandles.values(), ...thumbHandles.values(), ...nextHandles.values()]) {
      h.dispose();
      h.element.remove();
    }
    stageHandles.clear();
    thumbHandles.clear();
    nextHandles.clear();
  }

  /** A slide for the stage, rendered into the slot (hidden) if it isn't
   *  there yet. Slides must stay attached: the renderer finishes text layout
   *  asynchronously (fonts, autofit), and a detached slide measures as empty
   *  and loses its text. */
  function stageHandle(i: number, into: HTMLElement): SlideHandle | null {
    const hit = stageHandles.get(i);
    if (hit !== undefined) {
      stageHandles.delete(i);
      stageHandles.set(i, hit);
      return hit;
    }
    const made = render(i, true);
    if (made === null) return null;
    made.element.classList.add("stage-slide");
    made.element.style.visibility = "hidden";
    into.appendChild(made.element);
    stageHandles.set(i, made);
    while (stageHandles.size > STAGE_KEEP) {
      const [old, handle] = stageHandles.entries().next().value as [number, SlideHandle];
      if (old === i) break;
      handle.dispose();
      handle.element.remove();
      stageHandles.delete(old);
    }
    return made;
  }

  let slot = $state<HTMLDivElement | null>(null);

  // Show the current slide; then warm its neighbour in the idle time after.
  $effect(() => {
    const el = slot;
    const i = current;
    void deckGen;
    if (el === null || untrack(() => deck) === null) return;
    // The stage moves between the browsing layout and the presenter's: the
    // kept slides follow it into the new slot, laid out as they were.
    for (const h of stageHandles.values()) if (h.element.parentElement !== el) el.appendChild(h.element);
    const handle = untrack(() => stageHandle(i, el));
    for (const [j, h] of stageHandles) h.element.style.visibility = j === i ? "visible" : "hidden";
    if (handle === null) return;
    const warm = setTimeout(() => {
      if (i + 1 < untrack(() => count)) untrack(() => stageHandle(i + 1, el));
    }, 120);
    return () => clearTimeout(warm);
  });

  // `#slide=N`: on mount and whenever a reveal for this deck arrives.
  $effect(() => {
    void $revealRequest;
    const req = takeReveal(path);
    // A plain line (a check's problem on "slide N", `deck.pptx:3`) is a slide.
    const slideNo = req?.slide ?? req?.line;
    if (req === null || slideNo === undefined || !(slideNo >= 1)) return;
    const i = slideNo - 1;
    untrack(() => {
      revealed = req.region === undefined ? null : { slide: i, region: req.region };
      // The followed box replaces a pick, even on the slide already shown.
      if (revealed !== null) clearPick();
      if (deck === null) pendingSlide = i;
      else go(i);
    });
  });

  /** Land on slide `i`: in the show at its first state (a jump, as
   *  PowerPoint's), browsing as drawn. */
  function go(i: number): void {
    if (deck === null) return;
    asDrawn = true;
    show.go({ slide: Math.min(Math.max(0, i), count - 1), build: 0 });
  }

  /** Which slides the deck marks hidden (`show="0"`): read with each slide's
   *  timing at load, since the renderer knows only once it lays one out. */
  const hiddenFlags = $derived(
    deck?.presentation.slides.map((sl) => sl.hidden === true || extracts.get(sl.slidePath)?.hidden === true) ?? [],
  );
  /** Where the show stands among the slides it shows. */
  const showAt = $derived(showPosition(hiddenFlags, current));

  /** One click on or back. The show walks the slide's builds, then the
   *  next slide from its first state (back: the previous one at its last),
   *  skipping hidden slides as PowerPoint's does; browsing the deck walks
   *  every slide, each as drawn. */
  function page(delta: 1 | -1, auto = false): void {
    if (deck === null) return;
    if (!inShow) {
      go(current + delta);
      return;
    }
    // The controller parses the slides this click can land on first (a
    // slide not parsed yet counts as one build).
    show.step(delta, auto);
  }

  // --- builds ------------------------------------------------------------------------

  interface Timed {
    timeline: SlideTimeline;
    states: BuildState[];
  }

  /** Parsed timing per slide part, for the deck version on screen. */
  let timelines = new Map<string, Timed>();
  /** What each slide part's timeline needs, cut from it at load
   *  (`extractTiming`: its timing and transition, a few KB, capped), until
   *  it is parsed. The whole part goes: the renderer drops its own copy once
   *  it lays the slide out, and a deck's shapes can run to megabytes. */
  let extracts = new Map<string, TimingExtract>();
  /** Bumped when a timeline is parsed, so what counts builds re-reads. */
  let timelineGen = $state(0);
  /** Browsing shows the slide as drawn until a click dot is used. */
  let asDrawn = $state(true);
  /** The stage's build: the state after that many clicks; null is the slide
   *  as drawn. In the show and the presenter's view, the controller's. */
  const build = $derived(show.mode === "browse" && asDrawn ? null : show.at.build);

  /** Show build `k` of the current slide; null: the slide as drawn. */
  function setBuild(k: number | null): void {
    if (k === null) {
      asDrawn = true;
      return;
    }
    asDrawn = false;
    show.go({ slide: current, build: k });
  }
  /** Something on the current slide's builds could only be approximated. */
  let playApprox = $state(false);

  /** A new version: every slide's extract, before any is laid out (one scan
   *  a slide; the parse waits until the slide is asked for). */
  function resetTimelines(d: Deck): void {
    timelines = new Map();
    extracts = new Map();
    for (const sl of d.presentation.slides) {
      if (sl.sourceXml !== undefined) extracts.set(sl.slidePath, extractTiming(sl.sourceXml));
    }
  }

  /** Slide `i`'s timing, parsed on first ask. */
  function timelineOf(i: number): Timed | null {
    const sl = deck?.presentation.slides[i];
    if (sl === undefined) return null;
    const hit = timelines.get(sl.slidePath);
    if (hit !== undefined) return hit;
    const ex = extracts.get(sl.slidePath);
    let timeline: SlideTimeline;
    if (ex?.timeline != null) timeline = ex.timeline;
    else {
      const xml = ex?.source ?? untrack(() => edits.partText(sl.slidePath)) ?? sl.sourceXml ?? null;
      if (xml === null) return null;
      timeline = parseTimeline(xml);
    }
    const got = { timeline, states: buildStates(timeline) };
    timelines.set(sl.slidePath, got);
    // The hidden flag stays; the source is done with.
    if (ex !== undefined) extracts.set(sl.slidePath, { source: null, timeline: null, hidden: ex.hidden });
    return got;
  }

  /** Build counts per slide for `deckStep` (one where not parsed yet). */
  function buildCounts(): number[] {
    return deck?.presentation.slides.map((sl) => timelines.get(sl.slidePath)?.states.length ?? 1) ?? [];
  }

  // The current slide is always parsed (its click dots); in the show, the
  // slides a click can land on too, so a step back lands on the last build.
  $effect(() => {
    const i = current;
    const showing = inShow;
    const flags = hiddenFlags;
    void deckGen;
    void slideGen;
    untrack(() => {
      let fresh = false;
      for (const j of showing ? [i, ...showReach(flags, i, true)] : [i]) {
        const sl = deck?.presentation.slides[j];
        if (sl === undefined || timelines.has(sl.slidePath)) continue;
        if (timelineOf(j) !== null) fresh = true;
      }
      if (fresh) timelineGen += 1;
    });
  });

  /** The current slide's timing (parsed on first read: the controller may
   *  count a slide's builds before the parse effect gets to it). */
  const timed = $derived.by(() => {
    void timelineGen;
    void deckGen;
    void slideGen;
    void deck;
    return timelineOf(current);
  });
  /** The build on screen, held to the timeline (a re-read deck may have fewer). */
  const buildAt = $derived(build === null ? null : Math.min(build, Math.max(0, (timed?.states.length ?? 1) - 1)));
  /** Clicks on the current slide. */
  const clicks = $derived(Math.max(0, (timed?.states.length ?? 1) - 1));

  // Put the build on the stage slide: each redraw is re-tagged, and the
  // state goes back on it at once (no motion), so a deck re-read mid-show
  // keeps its place. Kept neighbours and edit mode show the slide as drawn.
  $effect(() => {
    const i = current;
    const b = buildAt;
    const t = timed;
    const editing = editMode;
    void deckGen;
    void slideGen;
    void slot;
    untrack(() => {
      let approx = false;
      for (const [j, handle] of stageHandles) {
        const tagged = stageTags.get(handle.element);
        if (tagged === undefined) continue;
        const state = j === i && b !== null && !editing ? t?.states[b] : undefined;
        if (state === undefined) clearState(tagged);
        else approx = applyState(tagged, state, w, h) || tagged.fellBack;
      }
      playApprox = approx;
    });
  });

  /** Why the current slide's builds are approximate, for the dots' tooltip. */
  const approxNote = $derived.by(() => {
    const code = timed?.timeline.approximate ?? null;
    if (code !== null) return `this slide's animation couldn't be read (${code}): it shows as a plain slide`;
    return playApprox ? "some builds on this slide are approximate: a shape or paragraph couldn't be matched to what was drawn" : "";
  });

  /** Show build `k` (a click dot); the build already shown goes back to the
   *  slide as drawn. */
  function showBuild(k: number): void {
    if (editMode) return;
    setBuild(!inShow && buildAt === k ? null : k);
  }

  /** The click dots: the slide's start, then one per click, at most DOTS
   *  of them in a window that follows the build (a deck of 300 clicks
   *  keeps a short row). */
  const DOTS = 8;
  const dotWindow = $derived.by(() => {
    if (clicks <= DOTS) return { first: 1, last: clicks };
    const first = Math.min(Math.max(1, (buildAt ?? 0) - Math.floor(DOTS / 2) + 1), clicks - DOTS + 1);
    return { first, last: first + DOTS - 1 };
  });

  let dotsEl = $state<HTMLSpanElement | null>(null);
  /** Arrows move along the dots (and the build with them); Escape is the
   *  slide as drawn again. */
  function onDotsKey(e: KeyboardEvent): void {
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    let to: number | null;
    if (e.key === "ArrowRight") to = Math.min(clicks, (buildAt ?? -1) + 1);
    else if (e.key === "ArrowLeft") to = Math.max(0, (buildAt ?? 1) - 1);
    else if (e.key === "Home") to = 0;
    else if (e.key === "End") to = clicks;
    else if (e.key === "Escape" && !inShow && build !== null) to = null;
    else return;
    e.preventDefault();
    e.stopPropagation();
    setBuild(to);
    void tick().then(() => dotsEl?.querySelector<HTMLElement>('[tabindex="0"]')?.focus());
  }

  // The show moves on by itself where the slide says (`advTm`), once its
  // last build is up; only while someone can see it, and any move restarts it.
  $effect(() => {
    if (!inShow || editMode || !onScreen.shown) return;
    const b = buildAt;
    const t = timed;
    const after = t?.timeline.transition?.advanceAfter ?? null;
    if (t === null || after === null || b === null || b < t.states.length - 1) return;
    const timer = setTimeout(() => {
      if (!disposed) page(1, true);
    }, after);
    return () => clearTimeout(timer);
  });


  // --- editing ---------------------------------------------------------------------

  const PRES_PART = "ppt/presentation.xml";
  const PRES_RELS = "ppt/_rels/presentation.xml.rels";

  const edits = $derived(deckEdits(path));
  $effect(() => {
    const e = edits;
    e.retainView();
    // A refused save rechecks the disk now rather than waiting for the watcher.
    e.onConflict = () => void file.current?.revalidate();
    return () => {
      e.onConflict = null;
      e.releaseView();
    };
  });

  let editMode = $state(false);
  let selectedShape = $state<string | null>(null);
  let texting = $state(false);
  /** The layer is editing the drawn slide (typing in place, mid-drag): don't
   *  redraw it under the caret or the pointer. */
  let holding = $state(false);
  /** A short note from the layer ("edit tables in PowerPoint"). */
  let layerNotice = $state<string | null>(null);
  /** Bumped when a slide was redrawn from edited XML (the layer re-reads it). */
  let slideGen = $state(0);
  let editLayer = $state<{ key(e: KeyboardEvent): boolean | "typing" } | null>(null);
  /** The version on screen, for a history's first attach. */
  let lastBytes: ArrayBuffer | null = null;
  /** The XML each part's slide was last drawn from, where that isn't disk. */
  const drawn = new Map<string, string>();
  /** `presentation.xml`'s relationship ids → slide parts (slide order). */
  let relTargets: Map<string, string> | null = null;
  /** PowerPoint has this deck open here (its `~$` owner file sits beside it). */
  let lockedBy = $state(false);
  let disposed = false;
  $effect(() => () => (disposed = true));

  async function attachEdits(bytes: ArrayBuffer, still: () => boolean): Promise<void> {
    const hash = await sha256Hex(bytes);
    if (!still() || disposed) return;
    await edits.attach(bytes, hash);
    if (!still() || disposed) return;
    await loadOrderParts();
    if (!still() || disposed) return;
    syncEdited();
  }

  /** The slide list and its relationships, read once per version. */
  async function loadOrderParts(): Promise<void> {
    if (relTargets !== null || !edits.ready) return;
    const [rels] = await Promise.all([edits.ensure(PRES_RELS), edits.ensure(PRES_PART)]);
    if (rels === null) return;
    const doc = new DOMParser().parseFromString(rels, "application/xml");
    const map = new Map<string, string>();
    for (const r of Array.from(doc.getElementsByTagNameNS("*", "Relationship"))) {
      const id = r.getAttribute("Id");
      const target = r.getAttribute("Target");
      if (id === null || target === null || r.getAttribute("TargetMode") === "External") continue;
      const base = target.startsWith("/") ? [] : ["ppt"];
      for (const seg of target.replace(/^\//, "").split("/")) {
        if (seg === "..") base.pop();
        else if (seg !== "." && seg !== "") base.push(seg);
      }
      map.set(id, base.join("/"));
    }
    relTargets = map;
  }

  /** Redraw every slide whose edited XML differs from what it shows, and
   *  follow an edited slide order. */
  function syncEdited(): void {
    const d = deck;
    if (d === null || !edits.ready) return;
    applyOrder(d);
    const now = deck;
    if (now === null) return;
    for (const [i, slide] of now.presentation.slides.entries()) {
      // The stage's slide is being edited in its own DOM (typing, a drag):
      // it is redrawn when that ends (the thumbnail follows then too).
      if (holding && i === current) continue;
      const text = edits.partText(slide.slidePath);
      if (text === null) continue;
      const was = drawn.get(slide.slidePath) ?? edits.diskText(slide.slidePath);
      if (text === was) continue;
      replaceSlide(i, text);
      drawn.set(slide.slidePath, text);
    }
  }

  $effect(() => {
    void edits.version;
    void holding;
    untrack(syncEdited);
  });

  /** Put the deck's slides in the edited order, the current slide kept. */
  function applyOrder(d: Deck): void {
    const xml = edits.partText(PRES_PART);
    const targets = relTargets;
    if (xml === null || targets === null) return;
    let order: string[];
    try {
      order = slideOrder(xml).map((r) => targets.get(r) ?? "");
    } catch {
      return;
    }
    const now = d.presentation.slides.map((sl) => sl.slidePath);
    if (order.length !== now.length || order.every((p, k) => p === now[k])) return;
    if (new Set(order).size !== order.length || order.some((p) => !now.includes(p))) return;
    const perm = order.map((p) => now.indexOf(p));
    const slides = perm.map((k) => d.presentation.slides[k]!);
    slides.forEach((sl, k) => (sl.index = k));
    d.presentation.slides.splice(0, slides.length, ...slides);
    const notesSkipped = new Set<number>();
    for (const k of d.notesSkipped) notesSkipped.add(perm.indexOf(k));
    disposeAll();
    itemsMemo = null;
    deck = { ...d, notes: perm.map((k) => d.notes[k] ?? ""), notesSkipped };
    deckGen += 1;
    // The controller stands on the slide by its part: it follows the move.
    show.relocate();
  }

  /** Draw slide `i` from `xml`: a fresh lazy slide in the presentation, its
   *  stage render swapped in place (new one up before the old goes, so the
   *  stage never blinks) and its thumbnail re-mounted. */
  function replaceSlide(i: number, xml: string): void {
    const d = deck;
    const old = d?.presentation.slides[i];
    if (d === null || old === undefined) return;
    const fresh: SlideData = {
      index: old.index,
      nodes: [],
      layoutIndex: old.layoutIndex,
      rels: old.rels,
      slidePath: old.slidePath,
      showMasterSp: true,
      sourceXml: xml,
      nodesMaterialized: false,
    };
    d.presentation.slides[i] = fresh;
    timelines.delete(old.slidePath);
    extracts.set(old.slidePath, extractTiming(xml));
    try {
      materializeSlideNodes(d.presentation, fresh);
    } catch {
      nodeErrors += 1;
    }
    itemsMemo = null;
    const th = thumbHandles.get(i);
    if (th !== undefined) {
      // The old picture stays until the new one replaces it (syncThumbs'
      // replaceChildren), so the strip never blinks empty.
      th.dispose();
      thumbHandles.delete(i);
    }
    const st = stageHandles.get(i);
    if (st !== undefined) {
      stageHandles.delete(i);
      const el = slot;
      if (el !== null && i === current) {
        const made = stageHandle(i, el);
        if (made !== null) made.element.style.visibility = "visible";
      }
      st.dispose();
      st.element.remove();
    }
    slideGen += 1;
  }

  /** The current slide in edit mode: laid out (its shapes known) and its
   *  part loaded, so the first gesture applies at once. */
  const editSlide = $derived.by(() => {
    void slideGen;
    void deckGen;
    return deck?.presentation.slides[current];
  });

  $effect(() => {
    // Also once the history attaches: entering edit mode starts the attach,
    // and the slide's part can only load after it.
    if (!editMode || !edits.ready) return;
    const i = current;
    void deckGen;
    const d = untrack(() => deck);
    const slide = d?.presentation.slides[i];
    if (d === null || slide === undefined) return;
    untrack(() => (selectedShape = null));
    try {
      materializeSlideNodes(d.presentation, slide);
    } catch {
      // Drawn as far as it goes; the layer shows what parsed.
    }
    void edits.ensure(slide.slidePath).then(() => {
      if (!disposed) slideGen += 1;
    });
  });

  async function toggleEdit(): Promise<void> {
    if (editMode) {
      editMode = false;
      selectedShape = null;
      stageEl?.focus({ preventScroll: true });
      return;
    }
    // The show window never edits; the presenter's view ends first, as the
    // show does (the slide as drawn is what an edit acts on).
    if (show.isShowWindow) return;
    if (show.mode === "presenter") show.presenter();
    if (areaMode) toggleArea();
    clearPick();
    revealed = null;
    asDrawn = true;
    editMode = true;
    stageEl?.focus({ preventScroll: true });
    void checkLock();
    const bytes = lastBytes;
    if (bytes !== null) await attachEdits(bytes, () => lastBytes === bytes);
  }

  async function checkLock(): Promise<void> {
    try {
      const listing = await fsList(dirname(path), true);
      lockedBy = listing.entries.some((e) => e.name === `~$${basename(path)}`);
    } catch {
      lockedBy = false;
    }
  }

  async function saveEdits(): Promise<void> {
    await edits.save();
  }

  function resolveConflicts(choice: "mine" | "theirs"): void {
    edits.resolve(choice);
    stageEl?.focus({ preventScroll: true });
  }

  /** Show the slide an undone or redone edit was on (the slide list stays). */
  function showPart(part: string | undefined): void {
    if (part === undefined || part === PRES_PART) return;
    const i = deck?.presentation.slides.findIndex((sl) => sl.slidePath === part) ?? -1;
    if (i >= 0 && i !== current) go(i);
  }

  function undoEdit(): void {
    showPart(edits.undo()?.part);
  }

  function redoEdit(): void {
    showPart(edits.redo()?.part);
  }

  /** "slide 1: 2 moves, text; slide 3: move": where the held edits are,
   *  grouped by slide and kind. */
  const conflictList = $derived.by(() => {
    const bySlide = new Map<string, Map<string, number>>();
    for (const c of edits.conflicts) {
      const i = deck?.presentation.slides.findIndex((sl) => sl.slidePath === c.part) ?? -1;
      const where = i >= 0 ? `slide ${i + 1}` : "the slide list";
      const kinds = bySlide.get(where) ?? new Map<string, number>();
      kinds.set(c.label, (kinds.get(c.label) ?? 0) + 1);
      bySlide.set(where, kinds);
    }
    const plural = (label: string, n: number) => (n === 1 ? label : `${n} ${label === "text" ? "text edits" : `${label}s`}`);
    return [...bySlide]
      .map(([where, kinds]) => `${where}: ${[...kinds].map(([l, n]) => plural(l, n)).join(", ")}`)
      .join("; ");
  });

  // --- slide order (thumbnail drag, edit mode) ---------------------------------------

  /** A thumbnail being dragged: `dx` is how far it moved within the strip,
   *  counting the strip's own scrolling since the press. */
  let thumbDrag = $state.raw<{
    from: number;
    pointer: number;
    startX: number;
    startScroll: number;
    clientX: number;
    dx: number;
    moved: boolean;
  } | null>(null);
  let thumbScrollRaf = 0;
  let swallowClick = false;
  /** Where the dragged slide lands: its index once moved (the others'
   *  centres it has passed). */
  const dropAt = $derived.by(() => {
    const t = thumbDrag;
    if (t === null || !t.moved) return null;
    const span = thumbW + THUMB_GAP;
    const centre = t.from * span + thumbW / 2 + t.dx;
    let n = 0;
    for (let k = 0; k < count; k++) if (k !== t.from && k * span + thumbW / 2 < centre) n++;
    return n;
  });
  /** The insertion mark's x in the strip, between the thumbnails it lands between. */
  const dropMarkX = $derived.by(() => {
    const t = thumbDrag;
    const to = dropAt;
    if (t === null || to === null || to === t.from) return null;
    const span = thumbW + THUMB_GAP;
    return (to < t.from ? to : to + 1) * span - THUMB_GAP / 2;
  });

  function onThumbDown(e: PointerEvent, i: number): void {
    if (!editMode || e.button !== 0 || !edits.ready || relTargets === null) return;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    thumbDrag = {
      from: i,
      pointer: e.pointerId,
      startX: e.clientX,
      startScroll: stripEl?.scrollLeft ?? 0,
      clientX: e.clientX,
      dx: 0,
      moved: false,
    };
  }

  function onThumbMove(e: PointerEvent): void {
    const t = thumbDrag;
    if (t === null || e.pointerId !== t.pointer) return;
    const dx = e.clientX - t.startX + ((stripEl?.scrollLeft ?? 0) - t.startScroll);
    if (!t.moved && Math.abs(dx) < 5) return;
    thumbDrag = { ...t, clientX: e.clientX, dx, moved: true };
    if (thumbScrollRaf === 0) thumbScrollRaf = requestAnimationFrame(autoScrollStrip);
  }

  /** Near either end of the strip a dragged thumbnail scrolls it, so a slide
   *  can travel the length of a long deck. */
  function autoScrollStrip(): void {
    thumbScrollRaf = 0;
    const t = thumbDrag;
    const strip = stripEl;
    if (t === null || !t.moved || strip === null) return;
    const r = strip.getBoundingClientRect();
    const edge = 40;
    const v = t.clientX < r.left + edge ? -(r.left + edge - t.clientX) : t.clientX > r.right - edge ? t.clientX - (r.right - edge) : 0;
    if (v === 0) return;
    const before = strip.scrollLeft;
    strip.scrollLeft = before + Math.max(-24, Math.min(24, v / 2));
    const moved = strip.scrollLeft - before;
    if (moved !== 0) thumbDrag = { ...t, dx: t.dx + moved };
    thumbScrollRaf = requestAnimationFrame(autoScrollStrip);
  }

  $effect(() => () => {
    if (thumbScrollRaf !== 0) cancelAnimationFrame(thumbScrollRaf);
  });

  function onThumbUp(e: PointerEvent): void {
    const t = thumbDrag;
    if (t === null || e.pointerId !== t.pointer) return;
    const el = e.currentTarget as HTMLElement;
    if (el.hasPointerCapture(e.pointerId)) el.releasePointerCapture(e.pointerId);
    const to = dropAt;
    thumbDrag = null;
    if (!t.moved || to === null) return;
    swallowClick = true;
    const xml = edits.partText(PRES_PART);
    if (xml === null || to === t.from) return;
    const from = slideOrder(xml);
    const next = [...from];
    const [moved] = next.splice(t.from, 1);
    next.splice(to, 0, moved!);
    // applyOrder keeps whichever slide is current, the dragged one included.
    edits.edit({ kind: "order", part: PRES_PART, from, to: next });
  }

  function onThumbCancel(e: PointerEvent): void {
    if (thumbDrag !== null && e.pointerId === thumbDrag.pointer) thumbDrag = null;
  }

  // --- the same file in PowerPoint ---------------------------------------------------

  const officeOk = $derived(canOpenInOffice(path));
  let trip = $state.raw<OfficeTrip | null>(null);
  let officeBusy = $state(false);
  let officeError = $state<string | null>(null);
  /** Where PowerPoint's saves go after "save mine as a copy". */
  let officeNote = $state<string | null>(null);
  let officeApp = $state("PowerPoint");

  $effect(() => {
    if (!officeOk) return;
    const p = path;
    let stop: (() => void) | null = null;
    let gone = false;
    void officeTrip(p).then((t) => {
      if (!gone) trip = t;
    });
    void listenOfficeTrip((t) => {
      // A trip ends on its own once PowerPoint closes the deck, all sent.
      if (t.path === p) trip = t.state === "done" ? null : t;
    }).then((un) => {
      if (gone) un();
      else stop = un;
    });
    return () => {
      gone = true;
      stop?.();
    };
  });

  async function openOffice(): Promise<void> {
    officeError = null;
    // PowerPoint opens what is on disk: unsaved edits here go first.
    if (edits.dirty) {
      const ok = await edits.save();
      if (!ok) {
        officeError = edits.conflicts.length > 0 ? "decide the conflicting edits first" : "save your edits first";
        return;
      }
    }
    officeBusy = true;
    try {
      const r = await openInOffice(path);
      officeApp = r.app;
      if (r.trip !== null) trip = r.trip;
    } catch (e) {
      officeError = e instanceof Error ? e.message : "PowerPoint couldn't open it";
    } finally {
      officeBusy = false;
    }
  }

  async function settleTrip(choice: "overwrite" | "copy" | "theirs" | "done"): Promise<void> {
    officeError = null;
    try {
      stageEl?.focus({ preventScroll: true });
      const copy = await resolveOfficeTrip(path, choice);
      // After "copy" the trip follows the copy: this deck has none left.
      if (choice === "done" || choice === "copy") trip = null;
      if (choice === "copy" && copy !== null) officeNote = `PowerPoint now saves to ${basename(copy)}`;
    } catch (e) {
      officeError = e instanceof Error ? e.message : "that didn't work";
    }
  }

  const tripLabel = $derived.by(() => {
    const t = trip;
    if (t === null) return null;
    if (t.state === "uploading") return "uploading…";
    if (t.state === "error") return "upload failed";
    if (t.state === "conflict") return null;
    if (t.uploaded_ms === null) return `in ${officeApp}`;
    const at = new Date(t.uploaded_ms);
    return `in ${officeApp} · saved ${at.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`;
  });

  let stageW = $state(0);
  let stageH = $state(0);
  // The presenter's view leaves room for the slide's focus ring.
  const pad = $derived(presenting ? 0 : show.mode === "presenter" ? 3 : 20);
  const scale = $derived(
    stageW > 0 && stageH > 0 ? Math.max(0.05, Math.min((stageW - pad * 2) / w, (stageH - pad * 2) / h)) : 0,
  );
  const thumbW = $derived(Math.round((THUMB_H * w) / h));
  const thumbScale = $derived(THUMB_H / h);

  // --- pointing at a slide, or part of one (context bridge) ------------------------

  /** The slide in points, and screen pixels per point on the stage. */
  const ptW = $derived(w * PX_TO_PT);
  const ptH = $derived(h * PX_TO_PT);
  const ptScale = $derived(scale / PX_TO_PT);

  /** The area tool is on: a drag on the slide draws a box, a click picks a shape. */
  let areaMode = $state(false);
  /** The box being drawn, then the one pointed at (points, on the current slide). */
  let pick = $state.raw<Region | null>(null);
  /** The box is drawn and published (its chip shows). */
  let pickReady = $state(false);
  /** What the chip's tooltip says is pointed at ("Chart 3 on slide 2"). */
  let pickLabel = $state("area");
  /** A gesture on the slide: where it started, and whether it became a drag. */
  let boxFrom: { at: Point; client: Point; pointer: number; moved: boolean } | null = null;
  const selOwner = {};
  let published: FileSelection | null = null;
  /** A `#slide=N&xywh=` reveal, outlined while its slide is on the stage. */
  let revealed = $state.raw<{ slide: number; region: Region & { percent?: boolean } } | null>(null);
  let boxEl = $state<HTMLDivElement | null>(null);
  const CHIP_W = 170;
  const CHIP_H = 28;

  /** The current slide's shapes, read once per slide per deck. */
  let itemsMemo: { deck: Deck; index: number; items: SlideItem[] } | null = null;
  function itemsOf(i: number): SlideItem[] {
    const d = deck;
    if (d === null) return [];
    if (itemsMemo?.deck !== d || itemsMemo.index !== i) itemsMemo = { deck: d, index: i, items: slideItems(d.presentation, i) };
    return itemsMemo.items;
  }

  function slidePick(): FileSelection | null {
    return deck === null ? null : slideReference(path, current + 1, itemsOf(current));
  }

  function slidePoint(e: PointerEvent): Point | null {
    const r = boxEl?.getBoundingClientRect();
    return r === undefined || !(ptScale > 0) ? null : screenToPage(e.clientX, e.clientY, { x: r.left, y: r.top }, ptScale);
  }

  function onBoxPointerDown(e: PointerEvent): void {
    if (e.button !== 0 || deck === null || inShow || editMode || !(areaMode || e.shiftKey)) return;
    const at = slidePoint(e);
    if (at === null) return;
    e.preventDefault();
    e.stopPropagation();
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    stageEl?.focus({ preventScroll: true });
    clearPick();
    revealed = null;
    window.getSelection()?.removeAllRanges();
    boxFrom = { at, client: { x: e.clientX, y: e.clientY }, pointer: e.pointerId, moved: false };
  }

  function onBoxPointerMove(e: PointerEvent): void {
    const from = boxFrom;
    if (from === null || e.pointerId !== from.pointer) return;
    // A few pixels of jitter is a click, not a box.
    if (!from.moved && Math.hypot(e.clientX - from.client.x, e.clientY - from.client.y) < 4) return;
    from.moved = true;
    const to = slidePoint(e);
    if (to !== null) pick = dragRegion(from.at, to, ptW, ptH);
  }

  function onBoxPointerUp(e: PointerEvent): void {
    const el = e.currentTarget as HTMLElement;
    if (el.hasPointerCapture(e.pointerId)) el.releasePointerCapture(e.pointerId);
    const from = boxFrom;
    if (from === null || e.pointerId !== from.pointer) return;
    boxFrom = null;
    let p = pick;
    if (p === null) {
      // A drag that spans nothing on the slide points at nothing.
      if (from.moved) return;
      // A click: the shape under the pointer, its own box.
      const hit = itemAt(itemsOf(current), from.at);
      p = hit === null ? null : snapBox(hit.box, ptW, ptH);
      if (p === null) return;
      pick = p;
    }
    const sel = areaReference(path, current + 1, itemsOf(current), p);
    published = sel;
    pickLabel = sel.label ?? "area";
    setSelection(selOwner, sel);
    pickReady = true;
  }

  /** The browser took the pointer mid-gesture: drop it, never publish half a box. */
  function onBoxPointerCancel(e: PointerEvent): void {
    const el = e.currentTarget as HTMLElement;
    if (el.hasPointerCapture(e.pointerId)) el.releasePointerCapture(e.pointerId);
    if (boxFrom === null || e.pointerId !== boxFrom.pointer) return;
    clearPick();
  }

  /** While pointing, a click on the slide is the gesture's own: a linked
   *  shape's jump or web link (the renderer's listeners, on the target)
   *  must not run. The slide's @ is a button of its own. */
  function onBoxClickCapture(e: MouseEvent): void {
    if (inShow || !(areaMode || e.shiftKey || editMode)) return;
    if ((e.target as Element | null)?.closest?.(".slide-ref") != null) return;
    e.preventDefault();
    e.stopPropagation();
  }

  function clearPick(): void {
    boxFrom = null;
    pick = null;
    pickReady = false;
    if (published !== null) {
      published = null;
      clearSelection(selOwner);
    }
  }

  function toggleArea(): void {
    // Pointing and editing both take the slide's pointer: one at a time.
    if (!areaMode && editMode) {
      editMode = false;
      selectedShape = null;
    }
    areaMode = !areaMode;
    if (!areaMode) clearPick();
    stageEl?.focus({ preventScroll: true });
  }

  // A newer selection elsewhere replaces this box.
  $effect(() => {
    const a = $activeSelection;
    if (published === null || a === published) return;
    published = null;
    pickReady = false;
    if (boxFrom === null) pick = null;
  });

  $effect(() => () => {
    if (published !== null) clearSelection(selOwner);
    published = null;
  });

  /** The revealed box in points (a `percent:` one is relative to the slide). */
  const revealBox = $derived.by(() => {
    const r = revealed;
    if (r === null || r.slide !== current || deck === null) return null;
    const { percent, ...raw } = r.region;
    const box = percent === true ? { x: (raw.x * ptW) / 100, y: (raw.y * ptH) / 100, w: (raw.w * ptW) / 100, h: (raw.h * ptH) / 100 } : raw;
    return snapBox(box, ptW, ptH);
  });

  /** The chip sits under the box's bottom-right corner, inside the stage. */
  const chipPos = $derived.by(() => {
    const p = pick;
    if (p === null || !pickReady || inShow) return null;
    const left0 = (stageW - w * scale) / 2;
    const top0 = (stageH - h * scale) / 2;
    const clamp = (n: number, lo: number, hi: number) => Math.min(Math.max(n, lo), Math.max(lo, hi));
    return {
      x: clamp(left0 + (p.x + p.w) * ptScale - CHIP_W, 4, stageW - CHIP_W),
      y: clamp(top0 + (p.y + p.h) * ptScale + 6, 4, stageH - CHIP_H - 4),
    };
  });

  let stageEl = $state<HTMLDivElement | null>(null);
  let stripEl = $state<HTMLDivElement | null>(null);

  // --- thumbnails -------------------------------------------------------------------

  let stripLeft = $state(0);
  let stripWidth = $state(0);
  const thumbRange = $derived.by(() => {
    const span = thumbW + THUMB_GAP;
    const first = Math.max(0, Math.floor(stripLeft / span) - THUMB_OVERSCAN);
    const last = Math.min(count - 1, Math.ceil((stripLeft + stripWidth) / span) + THUMB_OVERSCAN);
    return { first, last };
  });

  /** Drop the thumbnails out of range, then mount a couple of the missing
   *  ones; true when more are still missing. */
  function syncThumbs(): boolean {
    const strip = stripEl;
    if (strip === null || deck === null) return false;
    const { first, last } = thumbRange;
    for (const [i, handle] of thumbHandles) {
      if (i < first || i > last) {
        handle.dispose();
        handle.element.remove();
        thumbHandles.delete(i);
      }
    }
    // Nearest the current slide first; two per tick so paging stays smooth.
    const missing: number[] = [];
    for (let i = first; i <= last; i++) if (!thumbHandles.has(i)) missing.push(i);
    missing.sort((a, b) => Math.abs(a - current) - Math.abs(b - current));
    for (const i of missing.slice(0, 2)) {
      const box = strip.querySelector<HTMLElement>(`[data-thumb="${i}"] .thumb-slide`);
      if (box === null) continue;
      const handle = render(i);
      if (handle === null) continue;
      thumbHandles.set(i, handle);
      box.replaceChildren(handle.element);
    }
    return missing.length > 2;
  }

  $effect(() => {
    void thumbRange;
    void deckGen;
    void slideGen;
    if (stripEl === null) return;
    // After the stage: thumbnails never hold up the slide being read.
    let t = setTimeout(function pump() {
      if (untrack(syncThumbs)) t = setTimeout(pump, 16);
    }, 60);
    return () => clearTimeout(t);
  });

  // The strip moves between the browsing layout and the presenter's: the
  // thumbnails already drawn go into the new strip's boxes.
  $effect(() => {
    const strip = stripEl;
    if (strip === null) return;
    untrack(() => {
      stripLeft = strip.scrollLeft;
      for (const [i, handle] of thumbHandles) {
        if (strip.contains(handle.element)) continue;
        const box = strip.querySelector<HTMLElement>(`[data-thumb="${i}"] .thumb-slide`);
        if (box !== null) box.replaceChildren(handle.element);
        else {
          handle.dispose();
          handle.element.remove();
          thumbHandles.delete(i);
        }
      }
    });
  });

  $effect(() => {
    const strip = stripEl;
    if (strip === null) return;
    const ro = new ResizeObserver(() => (stripWidth = strip.clientWidth));
    ro.observe(strip);
    stripWidth = strip.clientWidth;
    return () => ro.disconnect();
  });

  $effect(() => {
    if (!showThumbs && show.mode !== "presenter") {
      for (const handle of thumbHandles.values()) handle.dispose();
      thumbHandles.clear();
    }
  });

  // Keep the current thumbnail in view as the deck is paged.
  $effect(() => {
    const strip = stripEl;
    const i = current;
    if (strip === null) return;
    const left = i * (thumbW + THUMB_GAP);
    const right = left + thumbW;
    if (left < strip.scrollLeft + 8) strip.scrollTo({ left: Math.max(0, left - 24), behavior: "smooth" });
    else if (right > strip.scrollLeft + strip.clientWidth - 8) {
      strip.scrollTo({ left: right - strip.clientWidth + 24, behavior: "smooth" });
    }
  });

  // --- the presenter's next click ------------------------------------------------

  /** What the next click shows: the same slide one build on, or the next
   *  shown slide before its first click; null at the end of the show. */
  const nextPlace = $derived.by(() => {
    void timelineGen;
    void deckGen;
    void slideGen;
    if (show.mode !== "presenter" || deck === null) return null;
    return show.next();
  });

  /** The next click drawn live: a second render of that slide, tagged and
   *  at that build. Two at most (this slide's next build and the next
   *  slide's start are often both wanted, back and forth). */
  const nextHandles = new Map<number, SlideHandle>();
  const NEXT_KEEP = 2;
  let nextSlot = $state<HTMLDivElement | null>(null);
  let nextW = $state(0);
  const nextScale = $derived(nextW > 0 ? nextW / w : 0);

  function disposeNext(): void {
    for (const h of nextHandles.values()) {
      h.dispose();
      h.element.remove();
    }
    nextHandles.clear();
  }

  $effect(() => {
    const el = nextSlot;
    const p = nextPlace;
    void deckGen;
    void slideGen;
    if (el === null || p === null) return;
    untrack(() => {
      let hd = nextHandles.get(p.slide);
      if (hd !== undefined && hd.element.parentElement !== el) {
        // Drawn into a slot that went (the view left and came back).
        hd.dispose();
        hd.element.remove();
        nextHandles.delete(p.slide);
        hd = undefined;
      }
      if (hd === undefined) {
        const made = render(p.slide, true);
        if (made === null) return;
        made.element.classList.add("stage-slide");
        // Never a link or a jump: the next click is only a picture.
        made.element.setAttribute("inert", "");
        el.appendChild(made.element);
        nextHandles.set(p.slide, made);
        hd = made;
        while (nextHandles.size > NEXT_KEEP) {
          const [old, h] = nextHandles.entries().next().value as [number, SlideHandle];
          h.dispose();
          h.element.remove();
          nextHandles.delete(old);
        }
      } else {
        nextHandles.delete(p.slide);
        nextHandles.set(p.slide, hd);
      }
      for (const [j, h] of nextHandles) h.element.style.visibility = j === p.slide ? "visible" : "hidden";
      const tagged = stageTags.get(hd.element);
      const state = timelineOf(p.slide)?.states[p.build];
      if (tagged !== undefined) {
        if (state === undefined) clearState(tagged);
        else applyState(tagged, state, w, h);
      }
    });
  });

  // Out of the presenter's view (or at the end of the show) the next
  // click's renders go, charts and listeners with them.
  $effect(() => {
    if (nextSlot === null) untrack(disposeNext);
  });

  /** Where the presenter stands: "3 / 12 · 2 of 4 clicks". */
  const presenterPlace = $derived(
    `${showAt.n} / ${showAt.of}${clicks > 0 ? ` · ${buildAt ?? 0} of ${clicks} ${clicks === 1 ? "click" : "clicks"}` : ""}`,
  );

  // --- keys, present -------------------------------------------------------------

  function onKey(e: KeyboardEvent): void {
    if (editMode && !presenting && deck !== null && !texting) {
      const mod = (e.metaKey || e.ctrlKey) && !e.altKey;
      if (!mod && !e.altKey) {
        const took = editLayer?.key(e) ?? false;
        if (took === "typing") return;
        if (took) {
          e.preventDefault();
          return;
        }
        // Editing, these keys are not the slide show's: no paging back on a
        // stray Backspace, no full screen on a typed f.
        if (["Backspace", "Delete", " ", "Enter", "f", "F5"].includes(e.key)) {
          e.preventDefault();
          return;
        }
      }
      if (!mod && e.key === "Escape") {
        e.preventDefault();
        void toggleEdit();
        return;
      }
    }
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    // Enter and Space on a control inside the stage (the slide's @, a
    // slide link) activate it; they page the deck only from the stage.
    if ((e.key === "Enter" || e.key === " ") && e.target !== e.currentTarget) return;
    if (deck === null) return;
    switch (e.key) {
      case "ArrowRight":
      case "ArrowDown":
      case "PageDown":
      case " ":
      case "Enter":
        page(e.shiftKey && e.key === " " ? -1 : 1);
        break;
      case "ArrowLeft":
      case "ArrowUp":
      case "PageUp":
      case "Backspace":
        page(-1);
        break;
      case "Home":
      case "End":
        // In the show, its first or last shown slide at its first state.
        if (inShow) show.edge(e.key === "Home" ? "first" : "last");
        else go(e.key === "Home" ? 0 : count - 1);
        break;
      case "f":
      case "F5":
        void togglePresent();
        break;
      case "p":
        if (presenting || editMode) return;
        void togglePresenter();
        break;
      case "Escape":
        if (inShow) void show.leave();
        else if (pick !== null) clearPick();
        else if (revealBox !== null) revealed = null;
        else if (areaMode) areaMode = false;
        else if (build !== null) setBuild(null);
        else return;
        break;
      default:
        return;
    }
    e.preventDefault();
  }

  /** ⌘S, ⌘Z and ⇧⌘Z / ⌘Y anywhere in the viewer (the stage, the strip after
   *  a drag, the bar's buttons), not only on the stage. A text field keeps
   *  its own undo. */
  function onViewKey(e: KeyboardEvent): void {
    // The presenter's view pages from anywhere in it (the strip, the timer),
    // not only from the stage, which handles its own keys first.
    if (show.mode === "presenter" && !e.defaultPrevented) {
      onKey(e);
      return;
    }
    if (!(e.metaKey || e.ctrlKey) || e.altKey || deck === null || inShow || texting) return;
    if (!(editMode || edits.dirty)) return;
    const t = e.target as HTMLElement | null;
    if (t !== null && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable)) return;
    const k = e.key.toLowerCase();
    if (k === "s") {
      e.preventDefault();
      void saveEdits();
    } else if (k === "z" || k === "y") {
      e.preventDefault();
      if (k === "y" || e.shiftKey) redoEdit();
      else undoEdit();
    }
  }

  async function togglePresent(): Promise<void> {
    if (presenting) {
      await show.leave();
      return;
    }
    // The show is never an editor: edit mode ends first, as its own button
    // or Esc would end it (unsaved edits stay where they are).
    if (editMode) await toggleEdit();
    await show.present();
  }

  async function togglePresenter(): Promise<void> {
    if (editMode) await toggleEdit();
    show.presenter();
    await tick();
    if (!disposed) stageEl?.focus({ preventScroll: true });
  }

  /** The show in a new window; this view, now the presenter's, keeps the
   *  keys (the controller ignores a second press while the window opens). */
  async function openShowWindow(): Promise<void> {
    await show.showInWindow();
    await tick();
    if (!disposed && show.mode === "presenter") stageEl?.focus({ preventScroll: true });
  }

  /** When a slide's own link last ran (the renderer's `onNavigate`): that
   *  click is the link's, never the show's. */
  let navigatedAt = -Infinity;
  /** Where a press on the show started: a drag is not a click. */
  let pressAt: { x: number; y: number } | null = null;

  /** A click in the show is the presenter's clicker: one build on. Not on
   *  a link, not when the press dragged or selected text. */
  function showClick(e: MouseEvent): void {
    const from = pressAt;
    pressAt = null;
    if (e.button !== 0 || performance.now() - navigatedAt < 400) return;
    if (from !== null && Math.hypot(e.clientX - from.x, e.clientY - from.y) > 4) return;
    const sel = window.getSelection();
    if (sel !== null && !sel.isCollapsed && stageEl?.contains(sel.anchorNode) === true) return;
    page(1);
  }

  /** Web links inside a slide open through the app, never in this page. */
  function onStageClick(e: MouseEvent): void {
    if (!inShow && (areaMode || e.shiftKey || editMode)) {
      e.preventDefault();
      return;
    }
    const a = (e.target as Element | null)?.closest?.("a[href]");
    if (a === null || a === undefined) {
      if (presenting) showClick(e);
      return;
    }
    e.preventDefault();
    const href = a.getAttribute("href") ?? "";
    if (/^https?:/i.test(href)) activateUrl(href, e.metaKey || e.ctrlKey);
  }

  const remote = isRemoteHost();
  let downloadError = $state<string | null>(null);
  async function download(): Promise<void> {
    downloadError = null;
    try {
      await fsDownload(path);
    } catch (e) {
      downloadError = e instanceof Error ? e.message : "download failed";
    }
  }

  const note = $derived(deck !== null && hasNotes ? (deck.notes[current] ?? "") : "");
  /** This slide's notes part was over the reader's cap and never inflated. */
  const noteSkipped = $derived(deck !== null && hasNotes && deck.notesSkipped.has(current));
  const hiddenSlide = $derived(deck?.presentation.slides[current]?.hidden === true);
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="pptx-view" bind:this={viewEl} onkeydown={onViewKey}>
  <div class="slides-bar">
    {#if deck !== null}
      <button class="bbtn icon" onclick={() => go(current - 1)} disabled={current === 0} title="previous slide (←)" aria-label="previous slide">
        <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true"><path d="M10 3.5 5.5 8 10 12.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" /></svg>
      </button>
      <span class="count" aria-live="polite">{current + 1} / {count}</span>
      <button class="bbtn icon" onclick={() => go(current + 1)} disabled={current >= count - 1} title="next slide (→)" aria-label="next slide">
        <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true"><path d="M6 3.5 10.5 8 6 12.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" /></svg>
      </button>
      {#if hiddenSlide}<span class="tag" title="this slide is hidden in the slide show">hidden</span>{/if}
      <span
        class="limits"
        title="drawn in the browser: builds appear and disappear click by click with no motion, no transitions, and fonts the deck doesn't embed fall back to this system's{deck.blocked > 0
          ? `; ${deck.blocked} linked outside ${deck.blocked === 1 ? 'resource was' : 'resources were'} not loaded`
          : ''}{nodeErrors > 0 ? `; ${nodeErrors} ${nodeErrors === 1 ? 'shape' : 'shapes'} couldn't be drawn` : ''}"
        aria-label="rendering limits"
      >
        <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true"
          ><circle cx="8" cy="8" r="6.2" fill="none" stroke="currentColor" stroke-width="1.3" /><path d="M8 7.2v4M8 4.9v.1" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" /></svg
        >
        {#if nodeErrors > 0 || deck.blocked > 0}<span>approximate</span>{/if}
      </span>
      <!-- One dot a click, last in the bar's left cluster so a row that
           grows or goes shifts nothing. The start mark is the slide before
           its first click; the dot of the build on screen, clicked again (or
           Escape), is the slide as drawn. -->
      {#if !editMode && clicks > 0}
        {@const tip = (label: string) => (approxNote === "" ? label : `${label} · ${approxNote}`)}
        <span
          class="builds"
          bind:this={dotsEl}
          role="group"
          aria-label="builds"
          title={approxNote || undefined}
        >
          <button
            class="pip start"
            class:on={buildAt !== null}
            class:at={buildAt === 0}
            tabindex={buildAt === null || buildAt === 0 ? 0 : -1}
            onclick={() => showBuild(0)}
            onkeydown={onDotsKey}
            title={tip(buildAt === 0 && !inShow ? "before the first click (click again: as drawn)" : "before the first click")}
            aria-label="before click 1 of {clicks}"
            aria-current={buildAt === 0 ? "step" : undefined}
          ><span aria-hidden="true"></span></button>
          {#each { length: dotWindow.last - dotWindow.first + 1 } as _, n (n)}
            {@const k = dotWindow.first + n}
            <button
              class="pip"
              class:on={buildAt !== null && k <= buildAt}
              class:at={buildAt === k}
              class:more={(n === 0 && k > 1) || (k === dotWindow.last && k < clicks)}
              tabindex={buildAt === k ? 0 : -1}
              onclick={() => showBuild(k)}
              onkeydown={onDotsKey}
              title={tip(buildAt === k && !inShow ? `click ${k} of ${clicks} (click again: as drawn)` : `click ${k} of ${clicks}`)}
              aria-label="click {k} of {clicks}"
              aria-current={buildAt === k ? "step" : undefined}
            ><span aria-hidden="true"></span></button>
          {/each}
        </span>
      {/if}
    {/if}
    {#if downloadError !== null}<span class="bar-err">{downloadError}</span>{/if}
    {#if officeError !== null}<span class="bar-err">{officeError}</span>{/if}
    {#if officeNote !== null}<span class="status" role="status">{officeNote}</span>{/if}
    {#if show.windowError !== null}<span class="bar-err" role="status">{show.windowError}</span>{/if}
    <span class="spacer"></span>
    {#if deck !== null && (editMode || edits.dirty || edits.saving)}
      <span class="edit-tools" role="group" aria-label="editing">
        {#if edits.blocked !== null}
          <span class="bar-err" title={edits.blocked}>can't edit: {edits.blocked}</span>
        {:else if !edits.ready}
          <span class="status">getting ready…</span>
        {:else}
          {#if edits.error !== null}<span class="bar-err" title={edits.error}>{edits.error}</span>{/if}
          {#if layerNotice !== null}<span class="status" role="status">{layerNotice}</span>{/if}
          {#if lockedBy}
            <span class="status warn" title="PowerPoint has this deck open: its next save there replaces what is saved here">open in PowerPoint</span>
          {/if}
          {#if edits.unjournaled && edits.dirty}
            <span class="status warn" title="your edits couldn't be backed up in this window's journal — save to keep them">not backed up</span>
          {/if}
          {#if edits.saving}
            <span class="status">saving…</span>
          {:else if edits.note !== null}
            <span class="status ok" role="status">{edits.note}</span>
          {:else if edits.dirty}
            <span class="status"><span class="dot" aria-hidden="true"></span>unsaved</span>
          {/if}
          <button class="bbtn icon" onclick={undoEdit} disabled={!edits.canUndo} title="undo (⌘Z)" aria-label="undo">
            <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true"><path d="M5.5 4 2.5 7l3 3M3 7h6.5a4 4 0 0 1 0 8H8" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" /></svg>
          </button>
          <button class="bbtn icon" onclick={redoEdit} disabled={!edits.canRedo} title="redo (⇧⌘Z)" aria-label="redo">
            <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true"><path d="M10.5 4l3 3-3 3M13 7H6.5a4 4 0 0 0 0 8H8" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" /></svg>
          </button>
          <button class="bbtn save" class:primed={edits.dirty} onclick={() => void saveEdits()} disabled={!edits.dirty || edits.saving} title="save (⌘S)">save</button>
        {/if}
      </span>
    {/if}
    {#if deck !== null}
      <!-- The presenter's view has its own notes and strip: their switches
           belong to browsing. -->
      {#if show.mode !== "presenter"}
        {#if hasNotes}
          <button class="bbtn" class:on={showNotes} onclick={() => (showNotes = !showNotes)} title="show or hide the speaker notes">notes</button>
        {/if}
        <button class="bbtn" class:on={showThumbs} onclick={() => (showThumbs = !showThumbs)} title="show or hide the thumbnail strip">thumbnails</button>
        <button
          class="bbtn icon area-btn"
          class:on={areaMode}
          onclick={toggleArea}
          aria-label="select an area"
          aria-pressed={areaMode}
          title={areaMode
            ? "drag a box on the slide, or click a shape, to reference it in an agent — click to stop (Esc)"
            : "select an area or a shape to reference in an agent (or Shift-drag)"}
        >
          <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true"
            ><path
              d="M2.5 5V3.5a1 1 0 0 1 1-1H5M11 2.5h1.5a1 1 0 0 1 1 1V5M13.5 11v1.5a1 1 0 0 1-1 1H11M5 13.5H3.5a1 1 0 0 1-1-1V11M7.2 2.5h1.6M7.2 13.5h1.6M2.5 7.2v1.6M13.5 7.2v1.6"
              fill="none"
              stroke="currentColor"
              stroke-width="1.3"
              stroke-linecap="round"
            /></svg
          >
        </button>
      {/if}
      {#if !show.isShowWindow}
        <button
          class="bbtn"
          aria-busy={show.opening}
          onclick={() => void openShowWindow()}
          title="the show in a new window (drag it to the projector); this one shows your notes, the next click and every slide"
          >show in window</button
        >
      {/if}
      <button
        class="bbtn"
        class:on={show.mode === "presenter"}
        onclick={() => void togglePresenter()}
        aria-pressed={show.mode === "presenter"}
        title="presenter view: this slide, the next click, your notes, a timer (P)">presenter</button
      >
      <button class="bbtn" onclick={() => void togglePresent()} title="present full screen (F)">present</button>
      {#if !show.isShowWindow}
        <button
          class="bbtn"
          class:on={editMode}
          onclick={() => void toggleEdit()}
          aria-pressed={editMode}
          title={editMode ? "stop editing (Esc)" : "edit: move, resize and retype shapes, drag thumbnails to reorder"}>edit</button
        >
      {/if}
    {/if}
    {#if officeOk}
      {#if tripLabel !== null}<span class="status" title="saves in PowerPoint are uploaded to {basename(path)} on this host">{tripLabel}</span>{/if}
      <button
        class="bbtn"
        onclick={() => void openOffice()}
        disabled={officeBusy}
        title={remote
          ? `open in PowerPoint: a copy on this Mac, every save there uploaded back${edits.dirty ? " (your edits here are saved first)" : ""}`
          : `open ${basename(path)} in PowerPoint${edits.dirty ? " (your edits here are saved first)" : ""}`}
        >{officeBusy ? "opening…" : "PowerPoint"}</button
      >
    {:else if remote}
      <button class="bbtn" onclick={() => void download()} title="download {basename(path)} to open it in PowerPoint">download</button>
    {/if}
  </div>

  {#if edits.conflicts.length > 0}
    <div class="banner" role="alert">
      <span>This deck changed on disk where you also edited it: {conflictList}.</span>
      <!-- The banner goes with the choice: focus goes back to the stage so the
           deck's keys (⌘S, arrows) keep working. -->
      <button class="opt" onclick={() => resolveConflicts("mine")}>keep mine</button>
      <button class="opt" onclick={() => resolveConflicts("theirs")}>use theirs</button>
    </div>
  {/if}
  {#if trip?.state === "conflict"}
    <div class="banner" role="alert">
      <span
        >{basename(path)} changed here while it was open in {officeApp}, so your last save there wasn't sent. Nothing
        was overwritten.</span
      >
      <button
        class="opt"
        onclick={() => void settleTrip("overwrite")}
        title="your {officeApp} version replaces the one here (the other changes are lost)">keep mine</button
      >
      <button
        class="opt"
        onclick={() => void settleTrip("copy")}
        title="both stay: yours is saved beside it as a copy, and later {officeApp} saves go to the copy">keep both</button
      >
      <button
        class="opt"
        onclick={() => void settleTrip("theirs")}
        title="opens the version here in {officeApp}; your unsent changes stay only in the window you have open">use theirs</button
      >
    </div>
  {:else if trip?.state === "error" && trip.message !== null}
    <div class="banner" role="alert"><span>{trip.message}</span></div>
  {/if}

{#snippet stageView()}
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div
    class="stage"
    class:presenting
    class:in-presenter={show.mode === "presenter"}
    bind:this={stageEl}
    bind:clientWidth={stageW}
    bind:clientHeight={stageH}
    tabindex="0"
    role="region"
    aria-label="slides"
    onkeydown={onKey}
    onclick={onStageClick}
    onpointerdown={(e) => {
      if (presenting) pressAt = { x: e.clientX, y: e.clientY };
      // The grey around the slide is "nothing": a press there deselects.
      if (editMode && e.target === e.currentTarget && !texting) selectedShape = null;
    }}
  >
    {#if deck !== null && scale > 0}
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div
        class="slide-box"
        bind:this={boxEl}
        style:width="{w * scale}px"
        style:height="{h * scale}px"
        onpointerdown={onBoxPointerDown}
        onmousedown={(e) => {
          // Shift extends a text selection by default; here it draws a box.
          // Typing in place: the press is the text's own (caret, selection).
          if (texting) return;
          if (areaMode || e.shiftKey || editMode) e.preventDefault();
        }}
        onpointermove={onBoxPointerMove}
        onpointerup={onBoxPointerUp}
        onpointercancel={onBoxPointerCancel}
        onclickcapture={onBoxClickCapture}
      >
        <div class="slide-scale" style:width="{w}px" style:height="{h}px" style:transform="scale({scale})" bind:this={slot}></div>
        {#if show.mode === "browse"}
          {#if editMode && edits.ready && edits.blocked === null && editSlide !== undefined}
            <PptxEditLayer
              bind:this={editLayer}
              {edits}
              slide={editSlide}
              part={editSlide.slidePath}
              slideW={w}
              slideH={h}
              {scale}
              version={slideGen + deckGen}
              bind:selected={selectedShape}
              bind:texting
              bind:holding
              bind:notice={layerNotice}
              slideEl={() => stageHandles.get(current)?.element ?? null}
              onsave={() => void saveEdits()}
            />
          {/if}
          {#if areaMode}<div class="area-layer" aria-hidden="true"></div>{/if}
          {#if revealBox !== null && pick === null}
            <div
              class="pick-mark revealed"
              aria-hidden="true"
              style:left="{revealBox.x * ptScale}px"
              style:top="{revealBox.y * ptScale}px"
              style:width="{revealBox.w * ptScale}px"
              style:height="{revealBox.h * ptScale}px"
            ></div>
          {/if}
          {#if pick !== null}
            <div
              class="pick-mark"
              class:drawing={!pickReady}
              aria-hidden="true"
              style:left="{pick.x * ptScale}px"
              style:top="{pick.y * ptScale}px"
              style:width="{pick.w * ptScale}px"
              style:height="{pick.h * ptScale}px"
            ></div>
          {/if}
          <span class="slide-ref" hidden={editMode}>
            <ReferenceButton pick={slidePick} label="slide {current + 1}" text="slide {current + 1}" />
          </span>
        {/if}
      </div>
      {#if chipPos !== null && pick !== null}
        <ReferenceChip x={chipPos.x} y={chipPos.y} label={pickLabel} />
      {/if}
      {#if presenting}
        <span class="present-count">{showAt.n} / {showAt.of}</span>
      {/if}
    {:else if error !== null}
      <div class="slides-error">
        <span>{error}</span>
        {#if tooLarge && remote}<button class="opt" onclick={() => void download()}>download</button>{/if}
      </div>
    {:else if deck === null}
      <Spinner label="reading the deck" />
    {/if}
    {#if error !== null && deck !== null}
      <div class="stale-note" role="status">{error} — showing the last good render</div>
    {/if}
  </div>
{/snippet}

<!-- Every slide, hidden ones dimmed: browsing opens one as drawn; the
     presenter's view jumps the show to its first state, and a show window
     follows. -->
{#snippet stripView()}
  <div class="strip" bind:this={stripEl} onscroll={() => stripEl !== null && (stripLeft = stripEl.scrollLeft)}>
    <div class="strip-inner" style:width="{count * (thumbW + THUMB_GAP) - THUMB_GAP}px" style:height="{THUMB_H}px">
      {#each { length: count } as _, i (i)}
        <button
          class="thumb"
          class:on={i === current}
          class:dim={deck?.presentation.slides[i]?.hidden === true}
          class:dragging={thumbDrag?.moved === true && thumbDrag.from === i}
          class:reorder={editMode}
          data-thumb={i}
          style:left="{i * (thumbW + THUMB_GAP)}px"
          style:width="{thumbW}px"
          style:transform={thumbDrag?.moved === true && thumbDrag.from === i ? `translateX(${thumbDrag.dx}px)` : undefined}
          onpointerdown={(e) => onThumbDown(e, i)}
          onpointermove={onThumbMove}
          onpointerup={onThumbUp}
          onpointercancel={onThumbCancel}
          ondragstart={(e) => e.preventDefault()}
          onclick={() => {
            if (swallowClick) {
              swallowClick = false;
              return;
            }
            go(i);
            stageEl?.focus();
          }}
          aria-label="slide {i + 1}"
          aria-current={i === current ? "true" : undefined}
        >
          <span class="thumb-slide" style:width="{w}px" style:height="{h}px" style:transform="scale({thumbScale})"></span>
          <span class="num">{i + 1}</span>
        </button>
      {/each}
      {#if dropMarkX !== null}<span class="drop-mark" aria-hidden="true" style:left="{dropMarkX}px"></span>{/if}
    </div>
  </div>
{/snippet}

<!-- The next click, drawn live at its build (`nextPlace`). -->
{#snippet nextView()}
  <div class="next-frame" bind:clientWidth={nextW}>
    {#if nextScale > 0}
      <div class="slide-scale" style:width="{w}px" style:height="{h}px" style:transform="scale({nextScale})" bind:this={nextSlot}></div>
    {/if}
  </div>
{/snippet}

  {#if show.mode === "presenter" && deck !== null}
    <PresenterView
      now={stageView}
      next={nextPlace === null ? null : nextView}
      strip={count > 1 ? stripView : undefined}
      ratio={w / h}
      notes={noteSkipped ? "" : note}
      noNotes={noteSkipped ? "This slide's notes are too large to show here." : undefined}
      elapsed={show.elapsed}
      onResetTimer={() => show.resetTimer()}
      place={presenterPlace}
      wallClock={show.wallClock}
    />
  {:else}
    {@render stageView()}

    {#if deck !== null && showNotes && (note !== "" || noteSkipped) && !presenting}
      {#if noteSkipped}
        <div class="notes skipped" role="note" aria-label="speaker notes">this slide's notes are too large to show here</div>
      {:else}
        <div class="notes" role="note" aria-label="speaker notes">{note}</div>
      {/if}
    {/if}

    {#if deck !== null && showThumbs && count > 1}
      {@render stripView()}
    {/if}
  {/if}
</div>

<style>
  .pptx-view {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
  }

  .slides-bar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.35rem;
    height: 26px;
    padding: 0 0.5rem 0 0.4rem;
    border-bottom: 1px solid var(--edge);
    font-size: var(--text-xs);
    color: var(--muted);
    min-width: 0;
  }

  .count {
    min-width: 4.5ch;
    text-align: center;
    font-family: var(--mono);
    font-variant-numeric: tabular-nums;
    color: var(--fg);
  }

  /* The click dots: as wide as their dots (at most the start mark and DOTS). */
  .builds {
    flex: none;
    display: inline-flex;
    align-items: center;
    height: 18px;
    margin-left: 0.25rem;
  }

  .pip {
    appearance: none;
    flex: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 10px;
    height: 18px;
    padding: 0;
    border: none;
    border-radius: 3px;
    background: none;
    cursor: pointer;
  }

  .pip > span {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: color-mix(in srgb, var(--fg) 22%, transparent);
    transition:
      background-color 0.12s ease,
      box-shadow 0.12s ease;
  }

  /* The start mark: a short bar, the slide before its first click. */
  .pip.start > span {
    width: 2px;
    height: 9px;
    border-radius: 1px;
  }

  /* More clicks beyond the window's edge. */
  .pip.more > span {
    width: 3px;
    height: 3px;
  }

  .pip:hover > span {
    background: color-mix(in srgb, var(--fg) 50%, transparent);
  }

  .pip.on > span {
    background: var(--accent);
  }

  .pip.at:not(.start) > span {
    box-shadow: 0 0 0 2px color-mix(in srgb, var(--accent) 28%, transparent);
  }

  .pip:focus-visible {
    outline: 2px solid var(--focus-ring);
    outline-offset: -1px;
  }

  .tag {
    padding: 0 0.4rem;
    border: 1px solid var(--edge);
    border-radius: 4px;
    font-size: 10px;
    letter-spacing: 0.03em;
  }

  .limits {
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    margin-left: 0.3rem;
    color: var(--muted);
    opacity: 0.8;
    cursor: help;
  }

  .bar-err {
    color: var(--err);
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .spacer {
    flex: 1;
  }

  .edit-tools {
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
    min-width: 0;
    padding-right: 0.35rem;
    margin-right: 0.15rem;
    border-right: 1px solid var(--edge);
  }

  .status {
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    white-space: nowrap;
    color: var(--muted);
  }

  .status.warn {
    color: var(--warn);
    cursor: help;
  }

  .status.ok {
    color: var(--accent-text);
  }

  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--fg);
    opacity: 0.6;
  }

  .bbtn.save.primed {
    color: var(--accent-text);
    background: color-mix(in srgb, var(--accent) 14%, transparent);
  }

  .bbtn.save.primed:hover:not(:disabled) {
    background: color-mix(in srgb, var(--accent) 24%, transparent);
  }

  .banner {
    flex: none;
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.4rem;
    padding: 0.35rem 0.6rem;
    border-bottom: 1px solid color-mix(in srgb, var(--warn) 35%, var(--edge));
    background: color-mix(in srgb, var(--warn) 8%, var(--bg));
    color: var(--fg);
    font-size: var(--text-xs);
  }

  .banner > span {
    flex: 1 1 18rem;
    min-width: 0;
  }

  .bbtn,
  .opt {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: pointer;
    padding: 0.1rem 0.45rem;
    border-radius: 4px;
    white-space: nowrap;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .opt {
    border: 1px solid var(--edge);
    padding: 0.2rem 0.7rem;
  }

  .bbtn.icon {
    display: inline-flex;
    align-items: center;
    padding: 0.2rem 0.3rem;
  }

  .bbtn:hover:not(:disabled),
  .opt:hover {
    background: var(--row-hover);
    color: var(--fg);
  }

  .bbtn.on {
    color: var(--fg);
  }

  .bbtn:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .stage {
    position: relative;
    flex: 1;
    min-height: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    overflow: hidden;
    background: color-mix(in srgb, var(--fg) 4%, var(--term-bg));
    outline: none;
  }

  .stage:focus-visible {
    box-shadow: inset 0 0 0 1px var(--focus-ring);
  }

  .stage.presenting {
    background: #000;
  }

  /* In the presenter's view the stage is the "now" picture: it fills its
     cell, on the view's own background. */
  .stage.in-presenter {
    align-self: stretch;
    background: none;
  }

  /* There the slide, not the empty cell around it, shows the keys' focus. */
  .stage.in-presenter:focus-visible {
    box-shadow: none;
  }

  .stage.in-presenter:focus-visible .slide-box {
    box-shadow:
      0 0 0 2px var(--focus-ring),
      0 2px 12px color-mix(in srgb, var(--fg) 14%, transparent);
  }

  /* The next click: the slide scaled to its frame, white like the stage's. */
  .next-frame {
    position: absolute;
    inset: 0;
    overflow: hidden;
    background: #fff;
  }

  /* The show is the slide and nothing else: no focus frame on the projector. */
  .stage.presenting:focus-visible {
    box-shadow: none;
  }

  .stage.presenting:not(:fullscreen) {
    position: fixed;
    inset: 0;
    z-index: 1000;
  }

  /* Clipped, never scrollable: the slide's unscaled layout box overflows it,
     and a scroll container would let a focus or scroll-into-view of the @
     inside it slide the deck sideways. */
  .slide-box {
    position: relative;
    flex: none;
    overflow: hidden;
    border-radius: 3px;
    background: #fff;
    box-shadow:
      0 0 0 1px color-mix(in srgb, var(--fg) 10%, transparent),
      0 2px 12px color-mix(in srgb, var(--fg) 14%, transparent);
  }

  @supports (overflow: clip) {
    .slide-box {
      overflow: clip;
    }
  }

  .presenting .slide-box {
    border-radius: 0;
    box-shadow: none;
  }

  /* Slides are authored on white in their own light scheme, and text the
     deck leaves unstyled is black — never the theme's inherited text color
     (light in a dark theme), font or spacing. */
  .slide-scale,
  .thumb-slide {
    color-scheme: light;
    color: #000;
    font: initial;
    line-height: normal;
    letter-spacing: normal;
    text-align: left;
    text-transform: none;
  }

  .slide-scale {
    position: relative;
    transform-origin: 0 0;
  }

  /* Kept slides stack in the slot; only the current one is visible. */
  .slide-scale :global(.stage-slide) {
    position: absolute !important;
    left: 0;
    top: 0;
  }

  /* The slide's @: shown while the stage is hovered or it has focus. */
  .slide-ref {
    position: absolute;
    top: 8px;
    right: 8px;
    z-index: 2;
    opacity: 0;
    transition: opacity 0.12s ease;
  }

  .stage:hover .slide-ref,
  .slide-ref:focus-within {
    opacity: 1;
  }

  /* The area tool takes the slide's pointer: links and slide jumps wait. */
  .area-layer {
    position: absolute;
    inset: 0;
    z-index: 1;
    cursor: crosshair;
  }

  /* The pointed-at area: solid once drawn, dashed while dragging. */
  .pick-mark {
    position: absolute;
    z-index: 1;
    box-sizing: border-box;
    pointer-events: none;
    border: 1.5px solid var(--accent);
    border-radius: 2px;
    background: color-mix(in srgb, var(--accent) 10%, transparent);
    box-shadow: 0 0 0 1px color-mix(in srgb, var(--term-bg) 70%, transparent);
  }

  .pick-mark.drawing {
    border-style: dashed;
    background: color-mix(in srgb, var(--accent) 6%, transparent);
  }

  /* A followed reference: outlined, not filled, so the slide reads through. */
  .pick-mark.revealed {
    background: none;
  }

  .bbtn.on.area-btn {
    color: var(--accent);
  }

  .present-count {
    position: absolute;
    right: 14px;
    bottom: 10px;
    font-family: var(--mono);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    color: #fff;
    opacity: 0.45;
  }

  .slides-error {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.5rem;
    color: var(--muted);
    font-size: var(--text-md);
    padding: 1rem;
    text-align: center;
  }

  .stale-note {
    position: absolute;
    left: 50%;
    bottom: 10px;
    transform: translateX(-50%);
    max-width: calc(100% - 20px);
    padding: 0.25rem 0.6rem;
    border: 1px solid color-mix(in srgb, var(--warn) 40%, var(--edge));
    border-radius: 6px;
    background: var(--overlay-bg);
    color: var(--warn);
    font-size: var(--text-xs);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .notes {
    flex: none;
    max-height: 22%;
    overflow: auto;
    scrollbar-width: thin;
    padding: 0.5rem 0.9rem;
    border-top: 1px solid var(--edge);
    background: var(--term-bg);
    color: var(--fg);
    font-size: var(--text-sm);
    line-height: 1.5;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .notes.skipped {
    color: var(--muted);
    font-style: italic;
  }

  .strip {
    flex: none;
    overflow-x: auto;
    overflow-y: hidden;
    scrollbar-width: thin;
    overscroll-behavior-x: contain;
    padding: 8px 10px;
    border-top: 1px solid var(--edge);
    background: var(--term-bg);
  }

  .strip-inner {
    position: relative;
    margin: 0 auto;
  }

  .thumb {
    position: absolute;
    top: 0;
    bottom: 0;
    appearance: none;
    padding: 0;
    border: 0;
    border-radius: 3px;
    overflow: hidden;
    background: #fff;
    cursor: pointer;
    box-shadow: 0 0 0 1px color-mix(in srgb, var(--fg) 12%, transparent);
    transition: box-shadow 0.12s ease;
  }

  .thumb.dim {
    opacity: 0.55;
  }

  .thumb.reorder {
    touch-action: none;
  }

  .thumb.dragging {
    z-index: 2;
    cursor: grabbing;
    transition: none;
    box-shadow:
      0 0 0 2px var(--accent),
      0 4px 14px color-mix(in srgb, #000 25%, transparent);
  }

  .drop-mark {
    position: absolute;
    top: -3px;
    bottom: -3px;
    width: 2px;
    margin-left: -1px;
    border-radius: 1px;
    background: var(--accent);
    pointer-events: none;
  }

  .thumb:hover {
    box-shadow: 0 0 0 1px color-mix(in srgb, var(--fg) 35%, transparent);
  }

  .thumb.on {
    box-shadow:
      0 0 0 2px var(--accent),
      0 0 0 3px color-mix(in srgb, var(--accent) 25%, transparent);
  }

  .thumb:focus-visible {
    outline: 2px solid var(--focus-ring);
    outline-offset: 2px;
  }

  .thumb-slide {
    position: absolute;
    left: 0;
    top: 0;
    display: block;
    transform-origin: 0 0;
    pointer-events: none;
  }

  .num {
    position: absolute;
    right: 3px;
    bottom: 2px;
    padding: 0 3px;
    border-radius: 3px;
    background: color-mix(in srgb, var(--term-bg) 80%, transparent);
    color: var(--muted);
    font-family: var(--mono);
    font-size: 9px;
    line-height: 1.4;
    font-variant-numeric: tabular-nums;
  }

  .thumb.on .num {
    color: var(--accent);
  }

  @media (prefers-reduced-motion: reduce) {
    .bbtn,
    .opt,
    .thumb,
    .slide-ref,
    .pip > span {
      transition: none;
    }
  }
</style>
