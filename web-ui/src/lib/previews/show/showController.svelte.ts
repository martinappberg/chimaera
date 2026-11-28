/**
 * A slide show's controller, for any slide viewer (the PowerPoint viewer's
 * today; it follows the plugin deck viewer's own presenter logic): the mode
 * (browsing, the show, the presenter's view), the place (slide and build),
 * the presenter's timer, two windows on one deck keeping step, and the show
 * in a window of its own for the projector. It holds no layout and draws
 * nothing: a viewer hands it a small deck adapter (`ShowDeck`) and is told
 * of every move (`arrive`), and draws the place itself.
 *
 * Two windows keep step only while a window is in the show or the
 * presenter's view, over a BroadcastChannel (`showSync.ts`): each move is
 * told as the slide's stable key and build. The presenter's view leads (as
 * does a window presenting on its own) and the show window follows
 * (`syncAction`): a leader joining tells its place, a follower joining
 * asks for the leader's. A window that is only browsing neither follows
 * nor leads. While leading, the presenter asks every few seconds whether a
 * show window is there (`followed`).
 *
 * Show in window: this window leaves an invite in localStorage and opens
 * its tab again in a new window; the new document, created after the
 * invite, takes it (once, while fresh) and starts its own show at the
 * place the invite names (where the presenter stands). A document
 * that already existed (the same deck in another pane) never starts a show
 * by itself; a window that does not open clears the invite, and this view
 * becomes the presenter's only once the window is open.
 *
 * The class creates its effects in the constructor: construct it during a
 * component's init (or inside `$effect.root`, as the tests do).
 */
import { tick, untrack } from "svelte";
import { deckStep, showEdge, showReach, type Place } from "../slideShow";
import { readSync, syncAction, type DeckSync } from "./showSync";

export type ShowMode = "browse" | "present" | "presenter";

/** What the controller needs to know of a deck. Called often: cheap. */
export interface ShowDeck {
  count(): number;
  /** The slides a show skips. */
  hidden(): readonly boolean[];
  /** Build counts per slide (a slide with none counts as one). The slides
   *  in `need` must be counted truly; a viewer that learns counts lazily
   *  may count the others as one. */
  builds(need: readonly number[]): readonly number[];
  /** A slide's stable key: what the windows name it by. */
  key(slide: number): string;
  /** The slide with this key (of several, the one nearest `near`): null
   *  when the deck has none. */
  find(key: string, near: number): number | null;
}

/** How the show arrived at a place. */
export interface Arrival {
  /** One click on in the show (a viewer may animate it). */
  click: boolean;
  /** The move came from another window. */
  heard: boolean;
  /** The show moved on by itself (an advance time, a clip that ended). */
  auto: boolean;
}

export interface ShowOptions {
  deck: ShowDeck;
  /** The channel two windows on this deck share (`syncName`). */
  sync: () => string;
  /** Someone can see this view (its pane and the page): the timer ticks
   *  only then. */
  visible: () => boolean;
  /** The element the show takes full screen (after the mode's update). */
  showEl: () => HTMLElement | null;
  /** Open this view's tab again in a new window; false when it didn't. */
  openWindow: () => Promise<boolean>;
  /** Browsing pages slide by slide (true) or build by build, as the show
   *  does with hidden slides walked. */
  browseBySlide: boolean;
  /** A jump to the show's last slide (End) lands on its last build (true)
   *  or on its first state, as PowerPoint's jumps do. */
  endOnLastBuild: boolean;
  /** Where the show or the presenter's view opens, from where browsing
   *  stood. Default: the show at the slide's first build, the presenter's
   *  view in place. */
  opening?: (mode: "present" | "presenter", at: Place) => Place;
  /** Every move, after `at` changed. */
  arrive?: (from: Place, to: Place, how: Arrival) => void;
  /** Back to browsing (the show or the presenter's view ended). */
  left?: () => void;
}

export const INVITE_KEY = "chimaera.deck.show-window";
export const INVITE_MS = 60_000;

/** How long a show window counts as there after it last answered. */
export const FOLLOWER_MS = 5_000;
const PING_MS = 2_000;

/** The invite a presenter leaves for the window it opens: the deck, when,
 *  and where the presenter stands (slide key and build). */
export function inviteText(sync: string, at: number, key: string, build: number): string {
  return JSON.stringify({ sync, at, key, build });
}

/** The place the invite `raw` names when it is for this deck, fresh at
 *  `now`, and made before this document was (`createdAt`,
 *  `performance.timeOrigin`); null when it isn't this document's. */
export function readInvite(raw: string | null, sync: string, now: number, createdAt: number): { key: string | null; build: number } | null {
  if (raw === null) return null;
  try {
    const v = JSON.parse(raw) as { sync?: unknown; at?: unknown; key?: unknown; build?: unknown };
    if (v.sync !== sync || typeof v.at !== "number" || now - v.at > INVITE_MS || now < v.at || createdAt < v.at) return null;
    const build = typeof v.build === "number" && Number.isInteger(v.build) && v.build >= 0 ? v.build : 0;
    return { key: typeof v.key === "string" && v.key !== "" ? v.key.slice(0, 2048) : null, build };
  } catch {
    return null;
  }
}

/** A place held to the deck: the slide in range, the build within it. */
export function clampPlace(builds: readonly number[], at: Place): Place {
  if (builds.length === 0) return { slide: 0, build: 0 };
  const slide = Math.min(Math.max(0, at.slide), builds.length - 1);
  const last = Math.max(1, builds[slide] ?? 1) - 1;
  return { slide, build: Math.min(Math.max(0, at.build), last) };
}

/** The show's next click from `at`: null at its end. */
export function nextClick(builds: readonly number[], hidden: readonly boolean[], at: Place): Place | null {
  const n = deckStep(builds, hidden, at, 1, true);
  return n.slide === at.slide && n.build === at.build ? null : n;
}

const same = (a: Place | null, b: Place): boolean => a !== null && a.slide === b.slide && a.build === b.build;

export class SlideShow {
  mode = $state<ShowMode>("browse");
  at = $state<Place>({ slide: 0, build: 0 });
  /** When the presenter's timer started, and the last tick. */
  started = $state(Date.now());
  now = $state(Date.now());
  /** The show's window is being opened. */
  opening = $state(false);
  /** Why the show's window didn't open. */
  windowError = $state<string | null>(null);
  /** This window is the show a presenter opened (until that show ends). */
  isShowWindow = $state(false);
  /** A show window answered within `FOLLOWER_MS` (this window leads). */
  followed = $state(false);

  readonly #o: ShowOptions;
  readonly #me = Math.random().toString(36).slice(2);
  /** The slide we stand on, by key: what a re-read deck is matched on. */
  #standing = "";
  #channel: BroadcastChannel | null = null;
  /** The show took the screen (as opposed to covering the window). */
  #wentFull = false;
  #disposed = false;
  #inviteChecked = false;
  /** The talk timer started once in this view's life: it keeps running
   *  through presenter off and on, the show and an edit. */
  #timerStarted = false;
  #followerAt = 0;
  /** In the show or the presenter's view: switching between the two keeps
   *  the channel (no new question, no jump). */
  readonly #inShow = $derived(this.mode !== "browse");

  constructor(o: ShowOptions) {
    this.#o = o;
    $effect(() => () => {
      this.#disposed = true;
    });

    // Two windows on one deck keep step while this one is in the show.
    $effect(() => {
      if (!this.#inShow || typeof BroadcastChannel === "undefined") return;
      const name = o.sync();
      if (name === "") return;
      const ch = new BroadcastChannel(name);
      ch.onmessage = (e: MessageEvent) => this.#hear(ch, e.data);
      this.#channel = ch;
      // Joining: a leader tells where the show is, a follower asks.
      untrack(() => this.#post(ch, this.#lead ? this.#where(false) : { kind: "where", from: this.#me }));
      return () => {
        ch.close();
        if (this.#channel === ch) this.#channel = null;
        this.followed = false;
      };
    });

    // The presenter asks every few seconds whether a show window is there
    // (its answers are ignored for the place); only while it can be seen.
    $effect(() => {
      if (this.mode !== "presenter" || !o.visible()) return;
      const ping = () => {
        if (this.#channel !== null) this.#post(this.#channel, { kind: "where", from: this.#me });
        this.followed = Date.now() - this.#followerAt < FOLLOWER_MS;
      };
      const id = setInterval(ping, PING_MS);
      untrack(ping);
      return () => clearInterval(id);
    });

    // "The show's window didn't open" goes after a while.
    $effect(() => {
      if (this.windowError === null) return;
      const id = setTimeout(() => (this.windowError = null), 8_000);
      return () => clearTimeout(id);
    });

    // The timer ticks only while someone can see it; the re-run on return
    // is the catch-up. A reset re-phases the ticks on the new start, so the
    // second it shows turns on the second.
    $effect(() => {
      void this.started;
      if (this.mode !== "presenter" || !o.visible()) return;
      this.now = Date.now();
      const id = setInterval(() => (this.now = Date.now()), 1000);
      return () => clearInterval(id);
    });

    // Leaving full screen by the browser's own key ends the show; only a
    // show that had the screen ends with it.
    $effect(() => {
      if (typeof document === "undefined") return;
      const onChange = () => {
        if (document.fullscreenElement === null && this.mode === "present" && this.#wentFull) {
          this.#wentFull = false;
          this.#toBrowse();
        }
      };
      document.addEventListener("fullscreenchange", onChange);
      return () => document.removeEventListener("fullscreenchange", onChange);
    });

    // The window a presenter opened takes the show as soon as it has slides.
    $effect(() => {
      if (this.#inviteChecked || o.deck.count() === 0) return;
      this.#inviteChecked = true;
      untrack(() => {
        const invite = this.#takeInvite();
        if (invite === null) return;
        this.isShowWindow = true;
        // The show opens where the presenter stands, before any message.
        const place = invite.key === null ? null : this.placeOf(invite.key, invite.build, 0);
        void this.present(place ?? undefined);
      });
    });
  }

  get inShow(): boolean {
    return this.mode !== "browse";
  }

  /** The presenter's running time, and the time of day. */
  get elapsed(): number {
    return this.now - this.started;
  }

  get wallClock(): string {
    return new Date(this.now).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  }

  /** This window leads the show: the presenter's view, or a show of its
   *  own (not one a presenter opened). */
  get #lead(): boolean {
    return this.mode === "presenter" || (this.mode === "present" && !this.isShowWindow);
  }

  /** Every mode change goes through here: a stale "didn't open" goes too. */
  #setMode(m: ShowMode): void {
    this.windowError = null;
    this.mode = m;
  }

  #toBrowse(): void {
    // A show window whose show ended is a viewer like any other.
    this.isShowWindow = false;
    this.#setMode("browse");
    this.#o.left?.();
  }

  /** The show's next click from where it stands: null at its end. */
  next(): Place | null {
    const deck = this.#o.deck;
    const hidden = deck.hidden();
    const at = this.at;
    return nextClick(deck.builds([at.slide]), hidden, at);
  }

  /** The slide called `key` at `build` (clamped), null when the deck has
   *  none: what another window's message names. */
  placeOf(key: string, build: number, near: number): Place | null {
    const i = this.#o.deck.find(key, near);
    if (i === null) return null;
    return clampPlace(this.#o.deck.builds([i]), { slide: i, build });
  }

  /** Go to `place` (clamped). `tell`: the other windows follow. */
  go(place: Place, { tell = true, click = false, auto = false, heard = false } = {}): void {
    const deck = this.#o.deck;
    if (deck.count() === 0) return;
    const to = clampPlace(deck.builds([Math.min(Math.max(0, place.slide), deck.count() - 1)]), place);
    const from = this.at;
    this.at = to;
    this.#standing = deck.key(to.slide);
    this.#o.arrive?.(from, to, { click, heard, auto });
    if (tell && this.mode !== "browse") this.#say();
  }

  /** One click on or back: the show walks builds and skips hidden slides;
   *  browsing walks every slide (slide by slide, or build by build). */
  step(delta: 1 | -1, auto = false): void {
    const deck = this.#o.deck;
    const at = this.at;
    if (!this.inShow && this.#o.browseBySlide) {
      this.go({ slide: Math.min(Math.max(0, at.slide + delta), deck.count() - 1), build: 0 });
      return;
    }
    const hidden = deck.hidden();
    const counts = deck.builds([at.slide, ...showReach(hidden, at.slide, this.inShow)]);
    this.go(deckStep(counts, hidden, at, delta, this.inShow), { click: delta === 1 && this.mode === "present", auto });
  }

  /** Home and End: the show's first or last shown slide; browsing, the deck's. */
  edge(end: "first" | "last"): void {
    const deck = this.#o.deck;
    const n = deck.count();
    if (n === 0) return;
    const i = this.inShow ? showEdge(deck.hidden(), end) : end === "first" ? 0 : n - 1;
    const last = end === "last" && this.#o.endOnLastBuild;
    this.go({ slide: i, build: last ? Math.max(1, deck.builds([i])[i] ?? 1) - 1 : 0 });
  }

  /** A re-read deck: stand on the same slide by key (it may have moved),
   *  else where we were, with the build clamped. */
  relocate(): void {
    const deck = this.#o.deck;
    if (deck.count() === 0) return;
    const place = (this.#standing === "" ? null : this.placeOf(this.#standing, this.at.build, this.at.slide)) ?? this.at;
    const to = clampPlace(deck.builds([Math.min(Math.max(0, place.slide), deck.count() - 1)]), place);
    if (to.slide !== this.at.slide || to.build !== this.at.build) this.at = to;
    this.#standing = deck.key(to.slide);
  }

  /** The show, at `start` (else where `opening` says). */
  async present(start?: Place): Promise<void> {
    start ??= this.#o.opening?.("present", this.at) ?? { slide: this.at.slide, build: 0 };
    this.#setMode("present");
    // A show opens a slide as a click would have: at its first build.
    this.go(start, { tell: false });
    await tick();
    const el = this.#o.showEl();
    if (el === null || this.#disposed) return;
    try {
      // Without full screen (a web view that refuses it, a window opened
      // without a gesture) the show still covers the window.
      await el.requestFullscreen?.();
      this.#wentFull = document.fullscreenElement === el;
    } catch {
      // Refused: the window-covering stage is the show.
    }
    el.focus({ preventScroll: true });
  }

  async leave(): Promise<void> {
    this.#wentFull = false;
    if (typeof document !== "undefined" && document.fullscreenElement !== null) await document.exitFullscreen().catch(() => {});
    if (this.mode === "browse") return;
    this.#toBrowse();
  }

  /** The presenter's view on, or off again. */
  presenter(): void {
    if (this.mode === "presenter") {
      this.#toBrowse();
      return;
    }
    if (this.mode === "present" && typeof document !== "undefined" && document.fullscreenElement !== null) {
      this.#wentFull = false;
      void document.exitFullscreen().catch(() => {});
    }
    const start = this.#presenterStart();
    if (!this.#timerStarted) this.resetTimer();
    this.isShowWindow = false;
    this.#setMode("presenter");
    this.go(start, { tell: false });
  }

  #presenterStart(): Place {
    return this.mode === "browse" ? (this.#o.opening?.("presenter", this.at) ?? this.at) : this.at;
  }

  resetTimer(): void {
    this.#timerStarted = true;
    this.started = Date.now();
    this.now = this.started;
  }

  /** The show in a new window (the projector's), this view the presenter's. */
  async showInWindow(): Promise<void> {
    this.windowError = null;
    if (this.opening) return;
    const sync = this.#o.sync();
    // The show window opens where the presenter's view will stand.
    const start = this.#presenterStart();
    try {
      localStorage.setItem(INVITE_KEY, inviteText(sync, Date.now(), this.#o.deck.key(start.slide), start.build));
    } catch {
      // No storage: the window opens on the deck, and F starts its show.
    }
    this.opening = true;
    const opened = await this.#o.openWindow().catch(() => false);
    this.opening = false;
    if (!opened) {
      // The layer flashed why (popup blocked, daemon unreachable); a view
      // with no layer to ask says so here.
      this.windowError = "The show's window didn't open.";
      try {
        localStorage.removeItem(INVITE_KEY);
      } catch {
        // Nothing was stored.
      }
      return;
    }
    if (!this.#disposed && this.mode !== "presenter") this.presenter();
  }

  #takeInvite(): { key: string | null; build: number } | null {
    try {
      const got = readInvite(localStorage.getItem(INVITE_KEY), this.#o.sync(), Date.now(), performance.timeOrigin);
      if (got !== null) localStorage.removeItem(INVITE_KEY);
      return got;
    } catch {
      return null;
    }
  }

  #where(reply: boolean): DeckSync {
    const msg: DeckSync = { kind: "at", from: this.#me, title: this.#o.deck.key(this.at.slide), slide: this.at.slide, build: this.at.build };
    if (this.#lead) msg.lead = true;
    if (reply) msg.reply = true;
    return msg;
  }

  #say(): void {
    if (this.#channel !== null) this.#post(this.#channel, this.#where(false));
  }

  #post(ch: BroadcastChannel, msg: DeckSync): void {
    try {
      ch.postMessage(msg);
    } catch {
      // A closed channel: the other window simply stops following.
    }
  }

  #hear(ch: BroadcastChannel, data: unknown): void {
    const msg = readSync(data);
    if (msg === null || msg.from === this.#me || this.#disposed) return;
    if (msg.kind === "where") {
      this.#post(ch, this.#where(true));
      return;
    }
    if (msg.lead !== true && this.#lead) {
      this.#followerAt = Date.now();
      this.followed = true;
    }
    const act = syncAction(this.#lead, msg);
    if (act === "ignore") return;
    const place = this.placeOf(msg.title, msg.build, msg.slide);
    if (place === null) return;
    // The show follows the presenter's clicker as a click: one click on
    // may animate, a jump lands.
    const click = this.mode === "present" && same(this.next(), place);
    this.go(place, { tell: act === "take-and-tell", click, heard: true });
  }
}
