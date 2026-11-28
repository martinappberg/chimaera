/**
 * A deck view's click motion, apart from its layout: which forward click
 * plays (the flick rule, reduced motion, only in the show), which pass of
 * the build-state effect plays it, the click playing now, the next click
 * planned while idle (`prepareStep`), and each slide's theme palette.
 * `PptxView` owns the stage and its tagged slides and asks this what to do;
 * nothing here touches layout.
 */
import { prepareStep, type Palette, type Playing, type PreparedStep, type PlayOptions } from "./pptxPlay";
import type { Deck } from "./pptxDeck";
import type { TaggedSlide } from "./pptxTag";
import { buildStates, extractTiming, parseTimeline, type BuildState, type SlideTimeline, type TimingExtract } from "./pptxTiming";

/** Forward clicks closer together than this land without playing (the
 *  plugin deck viewer's flick rule). */
export const FLICK_MS = 400;
/** How long the show stands still before the next click is planned. */
const WARM_MS = 200;

interface Place {
  slide: number;
  build: number;
}

/** The click playing on the stage (plain, never `$state`). */
interface PlayingClick extends Place {
  p: Playing;
  tagged: TaggedSlide;
  gen: string;
}

/** What `warm` needs of the view to plan the next click. */
interface WarmSource {
  /** Still in the show, mounted, nothing playing. */
  live(): boolean;
  next(): Place | null;
  tagged(slide: number): TaggedSlide | undefined;
  timeline(slide: number): { timeline: SlideTimeline; states: NonNullable<PlayOptions["states"]> } | null;
  options(slide: number): Pick<PlayOptions, "w" | "h" | "palette">;
}

/** Slide `i`'s theme colours and colour map: what a held colour that names
 *  a scheme colour (`accent2`, `tx1`) resolves against; kept per part. */
export function slidePalette(pres: Deck["presentation"] | undefined, i: number, cache: Map<string, Palette>): Palette | null {
  const sl = pres?.slides[i];
  if (pres === undefined || sl === undefined) return null;
  const hit = cache.get(sl.slidePath);
  if (hit !== undefined) return hit;
  const masterKey = pres.layoutToMaster.get(sl.layoutIndex);
  const master = masterKey === undefined ? undefined : pres.masters.get(masterKey);
  const themeKey = masterKey === undefined ? undefined : pres.masterToTheme.get(masterKey);
  const theme = themeKey === undefined ? undefined : pres.themes.get(themeKey);
  const map = new Map(master?.colorMap ?? []);
  if (sl.colorMapOverride !== undefined && sl.colorMapOverrideMode !== "master") for (const [k, v] of sl.colorMapOverride) map.set(k, v);
  const palette: Palette = { scheme: theme?.colorScheme ?? new Map(), map };
  cache.set(sl.slidePath, palette);
  return palette;
}

export class StageMotion {
  playing: PlayingClick | null = null;
  /** The click the next state pass plays rather than lands on. */
  toPlay: Place | null = null;
  /** The next forward click, planned while idle: a click then only starts
   *  animations. Dropped on every pass. */
  warmed: (Place & { gen: string; p: PreparedStep }) | null = null;
  /** The place the build-state pass last stood on. */
  lastPass: { slide: number; build: number | null } | null = null;
  lastClickAt = -Infinity;
  /** A click is playing: the advance timer waits for it to land. */
  animating = $state(false);
  #warmTimer: ReturnType<typeof setTimeout> | undefined;

  /** Land the click playing now, if any. */
  stop(): void {
    const p = this.playing;
    this.playing = null;
    p?.p.finish();
    if (this.animating) this.animating = false;
  }

  /** A move to `to`: only a click on in the show plays (the presenter's own
   *  view and every jump land at once); an advance by itself neither
   *  flicks nor counts as the click before the next. */
  moved(to: Place, how: { click?: boolean; auto?: boolean }, presenting: boolean, reduced: boolean, now: number): void {
    this.stop();
    this.toPlay = null;
    if (!how.click || !presenting) return;
    let animate = !reduced;
    if (!how.auto) {
      if (now - this.lastClickAt < FLICK_MS) animate = false;
      this.lastClickAt = now;
    }
    if (animate) this.toPlay = { slide: to.slide, build: to.build };
  }

  /** A build-state pass at (`i`, `b`): the click it plays, if any. A click
   *  plays only on the pass that brought its place: a re-read deck, a
   *  redraw or any later pass at the same place lands. */
  pass(i: number, b: number | null, showing: boolean): Place | null {
    const arrived = this.lastPass === null || this.lastPass.slide !== i || this.lastPass.build !== b;
    this.lastPass = { slide: i, build: b };
    const want = arrived && showing ? this.toPlay : null;
    this.toPlay = null;
    return want;
  }

  /** Whether the click playing is this very pass's (same place, version,
   *  drawing); anything else lands it. */
  owns(i: number, b: number | null, showing: boolean, editing: boolean, gen: string, tagged: TaggedSlide | undefined): boolean {
    const p = this.playing;
    return showing && p !== null && p.slide === i && p.build === b && !editing && p.gen === gen && tagged !== undefined && tagged === p.tagged;
  }

  /** The click planned ahead for (`i`, `b`) at this version, if any. */
  prepared(i: number, b: number, gen: string): PreparedStep | null {
    const w = this.warmed;
    return w !== null && w.slide === i && w.build === b && w.gen === gen ? w.p : null;
  }

  /** A click started playing; `onLanded` runs when it lands, if it is still
   *  the one playing. */
  started(mine: PlayingClick, onLanded: () => void): void {
    this.playing = mine;
    this.animating = true;
    void mine.p.finished.then(() => {
      if (this.playing !== mine) return;
      this.playing = null;
      onLanded();
    });
  }

  /** Plan the next forward click once the show stands still. */
  warm(gen: string, src: WarmSource, present: boolean): void {
    clearTimeout(this.#warmTimer);
    this.warmed = null;
    if (!present) return;
    this.#warmTimer = setTimeout(() => {
      if (!src.live() || this.playing !== null) return;
      const n = src.next();
      if (n === null) return;
      const tagged = src.tagged(n.slide);
      const t = src.timeline(n.slide);
      if (tagged === undefined || t === null) return;
      try {
        const p = prepareStep(tagged, t.timeline, n.build, { ...src.options(n.slide), states: t.states });
        if (p !== null) this.warmed = { slide: n.slide, build: n.build, gen, p };
      } catch {
        // Planned again at the click.
      }
    }, WARM_MS);
  }

  dispose(): void {
    clearTimeout(this.#warmTimer);
  }
}

/** A slide's parsed timing and the state after each build. */
export interface Timed {
  timeline: SlideTimeline;
  states: BuildState[];
}

/** A slide as the timings read it (the renderer's slide). */
interface TimedSlide {
  slidePath: string;
  sourceXml?: string;
}

/**
 * Every slide's timing for the deck version on screen: what each part's
 * timeline needs, cut from it at load (`extractTiming`: its timing and
 * transition, a few KB, capped), until it is parsed on first ask. The
 * whole part goes: the renderer drops its own copy once it lays the slide
 * out, and a deck's shapes can run to megabytes.
 */
export class SlideTimings {
  #parsed = new Map<string, Timed>();
  #extracts = new Map<string, TimingExtract>();

  /** A new version: every slide's extract, before any is laid out (one
   *  scan a slide; the parse waits until the slide is asked for). */
  reset(slides: readonly TimedSlide[]): void {
    this.#parsed = new Map();
    this.#extracts = new Map();
    for (const sl of slides) if (sl.sourceXml !== undefined) this.#extracts.set(sl.slidePath, extractTiming(sl.sourceXml));
  }

  /** One slide part redrawn from edited XML. */
  replace(path: string, xml: string): void {
    this.#parsed.delete(path);
    this.#extracts.set(path, extractTiming(xml));
  }

  /** Whether the part's own timing marks it hidden. */
  hidden(path: string): boolean {
    return this.#extracts.get(path)?.hidden === true;
  }

  has(path: string): boolean {
    return this.#parsed.has(path);
  }

  /** The slide's timing, parsed on first ask; `fallback` gives its XML
   *  where nothing was extracted (an edit's own text, the part). */
  of(sl: TimedSlide | undefined, fallback: () => string | null): Timed | null {
    if (sl === undefined) return null;
    const hit = this.#parsed.get(sl.slidePath);
    if (hit !== undefined) return hit;
    const ex = this.#extracts.get(sl.slidePath);
    let timeline: SlideTimeline;
    if (ex?.timeline != null) timeline = ex.timeline;
    else {
      const xml = ex?.source ?? fallback() ?? sl.sourceXml ?? null;
      if (xml === null) return null;
      timeline = parseTimeline(xml);
    }
    const got = { timeline, states: buildStates(timeline) };
    this.#parsed.set(sl.slidePath, got);
    // The hidden flag stays; the source is done with.
    if (ex !== undefined) this.#extracts.set(sl.slidePath, { source: null, timeline: null, hidden: ex.hidden });
    return got;
  }

  /** Build counts per slide for `deckStep` (one where not parsed yet). */
  counts(slides: readonly TimedSlide[]): number[] {
    return slides.map((sl) => this.#parsed.get(sl.slidePath)?.states.length ?? 1);
  }
}
