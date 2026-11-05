/**
 * A page shown in a pane by a native web view of its own (the app only).
 *
 * An `<iframe>` runs in this window's web process, on the one main thread
 * that also serves chat, terminals and every other pane: a 40 MB HTML report
 * cost that process 1–1.7 GB, tipped it into WebKit's memory-pressure cache
 * flushing, and froze the whole window (2026-11-04). The app can instead lay
 * a separate web view — its own process — over the pane
 * (`crates/chimaera-app/src/shell/native_view.rs`). This action keeps that
 * view on its placeholder element:
 *
 * - **Geometry.** The placeholder's rect is followed in short rAF bursts
 *   started by whatever can move it (a resize, the window's, an overlay, a
 *   drag, a transition), stopping once it holds still, plus a 1 s check
 *   while shown for a pure move nothing reports. Sent only when it moved.
 * - **Overlap.** A native view draws above every element of this page, so a
 *   menu, dialog, popover or tooltip over the pane would land under it. The
 *   view hides while a painted one overlaps and while a drag runs (drop
 *   zones), and the placeholder shows through.
 * - **Crash.** A view whose process died for good hides, and the host shows
 *   its error state (`onCrashed`) until a reload brings a page back.
 * - **Lifetime.** Hidden while `shown` is false (a parked tab, another mode),
 *   closed on destroy. A window reload drops every view (shell side).
 * - **Loading.** A new view, or one sent to a new URL, stays hidden until
 *   its page reports in (the bridge's first `page` event, at
 *   DOMContentLoaded) and the host shows its loading state meanwhile
 *   (`onLoading`): over a tunnel a big report takes seconds to arrive, and
 *   an empty white view would cover the spinner all that time.
 * - **Page events.** What the view reports (`native-view`: focus, a ⌘/Ctrl
 *   chord, its text selection, a click on the in-page reference pill, its
 *   URL and title) reaches the host through the `on…` params. A chord is
 *   replayed on `window` as a keydown, where App's chord engine (a capture
 *   listener, no `isTrusted` check) handles it as if pressed here: focus is
 *   in the view's own process, so this window never saw the key.
 * - **Control.** An action can't hand back a handle, so a host that drives
 *   the view (snapshot, the in-page pill, text under a rect, history,
 *   freezing it behind a stand-in) creates a `PaneViewControl` and passes it
 *   in; its calls are no-ops until the view exists.
 *
 * - **Embedded.** An embed card (`embedded`) sits in a scrolling document,
 *   not a pane. Its view is cropped to the part of its clipping ancestors
 *   on screen (the page keeps its place), hidden while its layer is parked
 *   (transparent) or once it is scrolled out, and the wheel over a `passive`
 *   one scrolls the document instead of the page (the shell hands it back:
 *   a view never chains a wheel the way an iframe does). A scroll of an
 *   ancestor follows the card frame by frame; with `stillWhileScrolling`
 *   (the host draws a still of the page in the placeholder, which scrolls
 *   in step) the view hides instead, and comes back once scrolling stops.
 *
 * `onFallback` fires when the shell can't show the view (outside the app, an
 * app build without pane views, a refused URL): the caller renders its
 * iframe instead.
 */

import {
  type ViewFit,
  isNativeShell,
  listenNativeView,
  nativeViewChip,
  nativeViewClose,
  nativeViewNav,
  nativeViewPlace,
  nativeViewRegion,
  nativeViewShow,
  nativeViewSnapshot,
  type NativeViewChip,
  type NativeViewNav,
  type NativeViewRegion,
  type NativeViewSnapshot,
  type ViewRect,
} from "../net/native";
import { chordInit, decodeNativeViewEvent, samePill, type NativeViewEvent } from "./paneViewBridge";

export interface PaneViewParams {
  /** Same-origin `/raw/…` or `/proxy/…` URL; null while it is being minted. */
  url: string | null;
  /** The pane layer is on screen and this view is the one it shows. Not the
   *  window's own visibility: the view sits in the same window and is hidden
   *  with it; hiding it too would only flash it on return. */
  shown: boolean;
  /** The shell can't host it: render an iframe instead. */
  onFallback?: () => void;
  /** A press or focus inside the view: focus its pane. */
  onFocus?: () => void;
  /** The page's text selection changed (`text` "" = cleared). `context` is
   *  the nearest heading above it; `rect` its last line box, view px. */
  onSelection?: (sel: { text: string; context: string | null; rect: ViewRect | null }) => void;
  /** The in-page reference pill was clicked. */
  onReference?: () => void;
  /** The page loaded, or its title or same-document URL changed. */
  onPage?: (page: { url: string; title: string }) => void;
  /** The view's own web process died twice within a minute and the shell
   *  stopped reloading it: show an error state with a way to `nav("reload")`
   *  (it clears with the next page). */
  onCrashed?: () => void;
  /** The view has yet to show this `url`'s page (it stays hidden until the
   *  page reports in): render a loading state in the placeholder. Called on
   *  each change, and once at mount. */
  onLoading?: (loading: boolean) => void;
  /** The host's handle on this view (see `PaneViewControl`). */
  control?: PaneViewControl;
  /** An embed card in a scrolling document (see "Embedded"). */
  embedded?: boolean;
  /** Embedded: the wheel over the view scrolls the document around it. */
  passive?: boolean;
  /** Embedded: hide the view while its document scrolls (the host shows a
   *  still of it in the placeholder); without one the view follows. */
  stillWhileScrolling?: boolean;
  /** The view came on screen (`true`) or was hidden: a snapshot is only
   *  worth taking while it is shown. */
  onShown?: (shown: boolean) => void;
}

/** The longest a loading view stays hidden waiting for its page. */
const LOAD_REVEAL_MS = 30_000;

const isBlank = (url: string): boolean => url === "about:blank" || url.startsWith("about:blank#");

/** Whether two URLs (absolute, or relative to this page) name one place. */
function sameUrl(a: string, b: string | null): boolean {
  if (b === null) return false;
  try {
    return new URL(a, location.href).href === new URL(b, location.href).href;
  } catch {
    return a === b;
  }
}

/**
 * What a transport lends its control while its page lives: the native view
 * here, or a framed page (`paneFrame.ts`, the bridge over `postMessage`).
 * Every method is safe to call once `live()`.
 */
export interface PaneTransport {
  /** The page can be reached (the view exists, the frame has a window). */
  live(): boolean;
  snapshot(): Promise<NativeViewSnapshot | null>;
  chip(chip: NativeViewChip | null): void;
  region(rect: ViewRect): Promise<NativeViewRegion | null>;
  nav(action: NativeViewNav): void;
  /** Re-evaluate placement now (a freeze flipped). */
  sync(): void;
}

/**
 * A host's handle on its pane page, whichever transport carries it. Create
 * one per page, pass it as `control`; every call is a no-op (or null)
 * until the page is reachable and after it went.
 */
export class PaneViewControl {
  private bound: PaneTransport | null = null;
  private pill: NativeViewChip | null = null;
  private held = false;

  /** @internal A transport took this page on. */
  bind(t: PaneTransport): void {
    this.bound = t;
  }

  /** @internal A transport let go (only its own claim: a successor that
   *  bound first keeps the control). */
  unbind(t: PaneTransport): void {
    if (this.bound === t) this.bound = null;
  }

  private reach(): PaneTransport | null {
    const t = this.bound;
    return t !== null && t.live() ? t : null;
  }

  /** @internal Re-send the pill to a page that (re)loaded. */
  applyChip(): void {
    const t = this.reach();
    if (t !== null && this.pill !== null) t.chip(this.pill);
  }

  /** The visible viewport as rendered now (native views only; null on a
   *  framed page). Take it BEFORE `freeze(true)`: a hidden view has
   *  nothing on screen to capture. */
  snapshot(): Promise<NativeViewSnapshot | null> {
    return this.reach()?.snapshot() ?? Promise.resolve(null);
  }

  /** The in-page reference pill (null removes it); kept and re-sent when
   *  the page loads anew. */
  setChip(chip: NativeViewChip | null): void {
    if (samePill(chip, this.pill)) return;
    this.pill = chip;
    this.reach()?.chip(chip);
  }

  /** The text under `rect` (viewport px) and the page's scroll then. */
  region(rect: ViewRect): Promise<NativeViewRegion | null> {
    return this.reach()?.region(rect) ?? Promise.resolve(null);
  }

  /** The page's own history. */
  nav(action: NativeViewNav): void {
    this.reach()?.nav(action);
  }

  /** Hide the view while the host shows something in its place (a
   *  snapshot to draw on); `false` puts it back. */
  freeze(on: boolean): void {
    if (this.held === on) return;
    this.held = on;
    this.bound?.sync();
  }

  get frozen(): boolean {
    return this.held;
  }
}

// One `native-view` listener per window, routing by view id. It stays for
// the window's life: one idle callback, and no subscribe/unsubscribe race
// when the last view closes as the next one opens.
const routes = new Map<string, (ev: NativeViewEvent) => void>();
let listening = false;

function route(id: string, fn: (ev: NativeViewEvent) => void): () => void {
  routes.set(id, fn);
  if (!listening) {
    listening = true;
    void listenNativeView((raw) => {
      const ev = decodeNativeViewEvent(raw);
      if (ev !== null) routes.get(ev.id)?.(ev);
    }).catch(() => (listening = false));
  }
  return () => {
    if (routes.get(id) === fn) routes.delete(id);
  };
}

/**
 * Whether pane views are worth trying here: the macOS app (the shell builds
 * them on WKWebView). The Linux and Windows apps go straight to the iframe
 * instead of paying a failing call per page.
 */
export function paneViewsPossible(): boolean {
  return isNativeShell() && /Mac/.test(navigator.userAgent);
}

/** Roles whose element floats over the workbench when positioned out of
 *  flow. Not `status`: live regions (a chat's screen-reader announcer) sit
 *  positioned and mounted for good, and would hide the view for good. */
const OVERLAY_SELECTOR = [
  '[role="menu"]',
  '[role="dialog"]',
  '[role="alertdialog"]',
  '[role="listbox"]',
  '[role="tooltip"]',
  "[data-overlay]",
].join(",");

/** An embedded view stays hidden this long after its document last
 *  scrolled (a wheel's momentum sends a scroll every frame until it ends). */
const SCROLL_SETTLE_MS = 140;
/** The embedded safety check: a card moves with no scroll and no resize of
 *  its own when the document above it reflows. */
const EMBEDDED_SAFETY_MS = 300;

/** Smaller than this is not something a person reads over the page. */
const OVERLAY_MIN_PX = 8;
/** Frames a geometry burst runs on after the rect stopped moving. */
const SETTLE_FRAMES = 10;
/** The safety check for a pure move nothing reported (a pane shifted by
 *  its neighbour without resizing), while shown on a visible window. */
const SAFETY_MS = 1000;

let seq = 0;

const sameRect = (a: ViewRect | null, b: ViewRect | null): boolean =>
  a !== null &&
  b !== null &&
  a.x === b.x &&
  a.y === b.y &&
  a.width === b.width &&
  a.height === b.height;

/** `a` cut to `b`, or null when less than a pixel of it is left. */
export function overlap(a: ViewRect, b: ViewRect): ViewRect | null {
  const x = Math.max(a.x, b.x);
  const y = Math.max(a.y, b.y);
  const right = Math.min(a.x + a.width, b.x + b.width);
  const bottom = Math.min(a.y + a.height, b.y + b.height);
  return right - x >= 1 && bottom - y >= 1 ? { x, y, width: right - x, height: bottom - y } : null;
}

/** Where a view should be: its rect, the visible part of its scroller
 *  (embedded only), and whether the wheel over it is the document's. */
interface Placement {
  rect: ViewRect;
  clip: ViewRect | null;
  passive: boolean;
}

const placementKey = (p: Placement | null): string =>
  p === null
    ? ""
    : `${p.rect.x},${p.rect.y},${p.rect.width},${p.rect.height}|${p.clip === null ? "" : `${p.clip.x},${p.clip.y},${p.clip.width},${p.clip.height}`}|${p.passive ? 1 : 0}`;

const fitOf = (p: Placement): ViewFit => (p.clip === null ? {} : { clip: p.clip, passive: p.passive });

/** Whether an element's overflow clips what it holds. */
function clipsContent(el: Element): boolean {
  const style = getComputedStyle(el);
  return style.overflowX !== "visible" || style.overflowY !== "visible" || /paint|strict|content/.test(style.contain);
}

const intersects = (a: DOMRect, b: ViewRect): boolean =>
  a.width >= OVERLAY_MIN_PX &&
  a.height >= OVERLAY_MIN_PX &&
  a.left < b.x + b.width &&
  a.right > b.x &&
  a.top < b.y + b.height &&
  a.bottom > b.y;

/** Whether an overlay candidate is actually painted: not in a parked
 *  (inert) layer, and not hidden by itself or an ancestor. */
function painted(el: Element): boolean {
  return el.closest("[inert]") === null && drawn(el);
}

/** Whether nothing hides an element: it, or an ancestor, is not
 *  `display:none`, `visibility:hidden` or fully transparent. */
function drawn(el: Element): boolean {
  const check = (el as Element & { checkVisibility?: (o: object) => boolean }).checkVisibility;
  if (check !== undefined) return check.call(el, { opacityProperty: true, visibilityProperty: true });
  // An older WebKit: walk up, since a parked layer hides its content
  // through an ancestor's opacity.
  for (let at: Element | null = el; at !== null; at = at.parentElement) {
    const style = getComputedStyle(at);
    if (style.display === "none" || style.opacity === "0") return false;
    if (at === el && style.visibility === "hidden") return false;
  }
  return true;
}

/**
 * Out-of-flow overlay elements outside `host`. The candidate list is
 * re-collected lazily after a DOM change that could add or drop one;
 * whether each is painted is read at check time (a layer parks by flipping
 * `inert`, which no childList change reports).
 */
class OverlayWatch {
  private dirty = true;
  private found: Element[] = [];
  private readonly observer: MutationObserver;
  private watching = false;

  /** `onChange` runs when the change may have moved an overlay. Observes
   *  only while `watch(true)`: a parked view has nothing to keep clear. */
  constructor(
    private readonly host: Element,
    onChange: () => void,
  ) {
    this.observer = new MutationObserver((records) => {
      if (this.relevant(records)) {
        this.dirty = true;
        onChange();
      }
    });
  }

  watch(on: boolean): void {
    if (on === this.watching) return;
    this.watching = on;
    if (on) {
      // Whatever came and went while unwatched is re-collected.
      this.dirty = true;
      // `inert` flips park and un-park layers (a parked overlay stops
      // counting; an embedded view's own layer hides it).
      this.observer.observe(document.body, { childList: true, subtree: true, attributes: true, attributeFilter: ["inert"] });
    } else {
      this.observer.disconnect();
    }
  }

  /** Whether a batch of DOM changes added or removed an overlay: streaming
   *  chat text mutates the tree constantly, and none of that may wake the
   *  geometry loop. */
  private relevant(records: MutationRecord[]): boolean {
    if (this.found.some((el) => !el.isConnected)) return true;
    for (const r of records) {
      if (r.type === "attributes") return true;
      for (const n of r.addedNodes) {
        if (n instanceof Element && (n.matches(OVERLAY_SELECTOR) || n.querySelector(OVERLAY_SELECTOR) !== null)) {
          return true;
        }
      }
    }
    return false;
  }

  covering(rect: ViewRect): boolean {
    if (this.dirty) {
      this.dirty = false;
      this.found = [...document.querySelectorAll(OVERLAY_SELECTOR)].filter((el) => {
        if (this.host.contains(el) || el.contains(this.host)) return false;
        const position = getComputedStyle(el).position;
        return position === "fixed" || position === "absolute";
      });
    }
    return this.found.some((el) => el.isConnected && intersects(el.getBoundingClientRect(), rect) && painted(el));
  }

  stop(): void {
    this.observer.disconnect();
  }
}

/** `use:paneView={{ url, shown, … }}` on the placeholder element. */
export function paneView(node: HTMLElement, initial: PaneViewParams) {
  const id = `v${(seq += 1)}-${Math.random().toString(36).slice(2, 8)}`;
  let params = initial;
  let created = false;
  let failed = false;
  let destroyed = false;
  /** What the shell shows now (`placementKey`; "" = hidden). */
  let placed = "";
  /** The URL the shell was last asked to show. */
  let showing: string | null = null;
  let dragging = false;
  /** One shell call at a time, newest wins: rapid moves coalesce. */
  let busy = false;
  let again = false;
  /** A load is under way: the view stays hidden and the host shows its
   *  loading state, until the page reports in or `LOAD_REVEAL_MS` passes. */
  let awaiting = false;
  /** The URL navigated away from: its late events are not the new page. */
  let leaving: string | null = null;
  let loadTimer: ReturnType<typeof setTimeout> | null = null;
  let reported: boolean | null = null;
  /** The view's own process died for good (the shell stopped reloading):
   *  hidden, so the host's error state shows, until a page reports in. */
  let crashed = false;
  /** Embedded: its document is scrolling and the view stands down for the
   *  host's still. */
  let scrolling = false;
  let scrollTimer: ReturnType<typeof setTimeout> | null = null;
  /** Embedded: the ancestors whose overflow crops it, nearest first
   *  (re-collected after a resize: cards don't move between parents). */
  let clippers: Element[] | null = null;

  const loadingNow = (): boolean =>
    !failed && !crashed && params.url !== null && (awaiting || showing !== params.url);
  const report = (): void => {
    const now = loadingNow();
    if (now === reported || destroyed) return;
    reported = now;
    params.onLoading?.(now);
  };
  const clearLoadTimer = (): void => {
    if (loadTimer !== null) clearTimeout(loadTimer);
    loadTimer = null;
  };
  const landed = (): void => {
    awaiting = false;
    clearLoadTimer();
    report();
    burst();
  };
  // One-shot, not a poll: a page that never reaches DOMContentLoaded (a
  // hung asset, a bridge that didn't load) is shown as it is rather than
  // hidden for good.
  const armLoadTimer = (): void => {
    clearLoadTimer();
    loadTimer = setTimeout(() => {
      loadTimer = null;
      if (awaiting && !destroyed) landed();
    }, LOAD_REVEAL_MS);
  };
  const awaitPage = (from: string | null): void => {
    leaving = from;
    awaiting = true;
    armLoadTimer();
    report();
  };

  const onEvent = (ev: NativeViewEvent): void => {
    switch (ev.kind) {
      case "focus":
        params.onFocus?.();
        break;
      case "key":
        window.dispatchEvent(new KeyboardEvent("keydown", chordInit(ev)));
        break;
      case "selection":
        params.onSelection?.({ text: ev.text, context: ev.context, rect: ev.rect });
        break;
      case "reference":
        params.onReference?.();
        break;
      case "page":
        // Every view starts on about:blank before the shell points it at
        // its URL: that is not the page.
        if (isBlank(ev.url)) break;
        if (crashed) {
          crashed = false;
          landed();
        } else if (awaiting && !sameUrl(ev.url, leaving)) {
          landed();
        }
        // A new document starts without the pill: give it back.
        control?.applyChip();
        params.onPage?.({ url: ev.url, title: ev.title });
        break;
      case "crashed":
        crashed = true;
        awaiting = false;
        clearLoadTimer();
        report();
        burst();
        params.onCrashed?.();
        break;
    }
  };

  const unroute = route(id, onEvent);
  const bound: PaneTransport = {
    live: () => created && !destroyed && !failed,
    snapshot: () => nativeViewSnapshot(id),
    chip: (chip) => void nativeViewChip(id, chip),
    region: (rect) => nativeViewRegion(id, rect),
    nav: (action) => {
      // Reloading a crashed page: hidden behind the loading state until
      // the new process reports in, like any load.
      if (action === "reload" && crashed) {
        crashed = false;
        awaitPage(null);
      }
      void nativeViewNav(id, action);
    },
    sync: () => burst(),
  };
  let control = initial.control ?? null;
  control?.bind(bound);

  const measure = (): ViewRect | null => {
    const r = node.getBoundingClientRect();
    if (r.width < 1 || r.height < 1) return null;
    return { x: Math.round(r.left), y: Math.round(r.top), width: Math.round(r.width), height: Math.round(r.height) };
  };

  /** Embedded: the part of the window its clipping ancestors leave
   *  visible (each one's padding box, scrollbars excluded). */
  const visibleClip = (): ViewRect => {
    if (clippers === null) {
      clippers = [];
      for (let p = node.parentElement; p !== null && p !== document.documentElement; p = p.parentElement) {
        if (clipsContent(p)) clippers.push(p);
      }
    }
    let left = 0;
    let top = 0;
    let right = window.innerWidth;
    let bottom = window.innerHeight;
    for (const el of clippers) {
      const box = el.getBoundingClientRect();
      const x = box.left + el.clientLeft;
      const y = box.top + el.clientTop;
      left = Math.max(left, x);
      top = Math.max(top, y);
      right = Math.min(right, x + el.clientWidth);
      bottom = Math.min(bottom, y + el.clientHeight);
    }
    return {
      x: Math.round(left),
      y: Math.round(top),
      width: Math.max(0, Math.round(right - left)),
      height: Math.max(0, Math.round(bottom - top)),
    };
  };

  /** Where the view should be right now: null = hidden. */
  const wanted = (): Placement | null => {
    if (!params.shown || params.url === null || dragging || awaiting || crashed || control?.frozen === true) {
      return null;
    }
    const embedded = params.embedded === true;
    // A pane gets its layer's state as `shown`; a card reads it itself: a
    // parked layer is transparent (and inert, whose flip wakes the overlay
    // watch). Not `inert` alone: the hover preview's content is inert and
    // on screen.
    if (embedded && ((scrolling && params.stillWhileScrolling === true) || !drawn(node))) {
      return null;
    }
    const rect = measure();
    if (rect === null) return null;
    const clip = embedded ? visibleClip() : null;
    const seen = clip === null ? rect : overlap(rect, clip);
    if (seen === null || overlays.covering(seen)) return null;
    return { rect, clip, passive: embedded && params.passive === true };
  };

  async function sync(): Promise<void> {
    if (busy) {
      again = true;
      return;
    }
    busy = true;
    try {
      do {
        again = false;
        if (destroyed || failed) return;
        const want = wanted();
        const url = params.url;
        // A card is made only once it could be shown: one in a parked tab
        // or scrolled past (still "shown" to its observer) loads nothing.
        const ready = params.embedded !== true || want !== null;
        if (!crashed && params.shown && ready && !dragging && url !== null && (!created || url !== showing)) {
          // A create or a new URL: the shell makes or points the view
          // hidden, and it stays so until the page reports in (set before
          // the call: a fast page can report before the call returns).
          const at = want?.rect ?? measure();
          if (at === null) continue;
          awaitPage(created ? showing : null);
          const embedded = params.embedded === true;
          const fit: ViewFit = embedded ? { clip: visibleClip(), passive: params.passive === true } : {};
          const wasShown = placed !== "";
          const shown = await nativeViewShow(id, url, at, true, fit);
          // The view exists from here even if this action is gone: the
          // `finally` closes it then.
          if (shown !== null) created = true;
          if (destroyed) break;
          // A card needs the crop and the wheel hand-back: a shell that
          // ignored them (an older app build) would draw it over the
          // window's chrome, so the card keeps its frame instead.
          const ok = shown !== null && (!embedded || shown.fit);
          if (shown !== null && !ok) {
            created = false;
            void nativeViewClose(id);
          }
          if (!ok) {
            failed = true;
            awaiting = false;
            clearLoadTimer();
            params.onFallback?.();
            return;
          }
          showing = url;
          placed = "";
          // A new URL loads hidden: the view the host saw is gone for now.
          if (wasShown) params.onShown?.(false);
          report();
          control?.applyChip();
          // The page beat the call: place it on the next pass.
          if (!awaiting) again = true;
        } else if (created && placementKey(want) !== placed) {
          const was = placed !== "";
          placed = placementKey(want);
          await (want === null ? nativeViewPlace(id, null) : nativeViewPlace(id, want.rect, fitOf(want)));
          if (was !== (want !== null) && !destroyed) params.onShown?.(want !== null);
        }
      } while (again && !destroyed);
    } finally {
      busy = false;
      // Closed while a call was in flight (a create the shell finished
      // after the close went): close again now.
      if (destroyed && created) void nativeViewClose(id);
    }
  }

  // Geometry runs in bursts, not a standing 60 Hz loop: whatever can move
  // the placeholder (a resize, a window resize, an overlay coming or
  // going, a drag, a transition, a param change) starts a few frames of
  // following, which stop once the rect has held still for SETTLE_FRAMES.
  let frame = 0;
  let still = 0;
  let lastRect: ViewRect | null = null;
  const tick = (): void => {
    frame = 0;
    if (destroyed || failed) return;
    const rect = measure();
    if (sameRect(rect, lastRect) || (rect === null && lastRect === null)) still += 1;
    else still = 0;
    lastRect = rect;
    void sync();
    if (still < SETTLE_FRAMES) frame = requestAnimationFrame(tick);
  };
  function burst(): void {
    if (destroyed) return;
    // A parked view has no rect to follow: one pass hides it (or keeps it
    // hidden), and the document's transitions and resizes cost it nothing.
    if (!params.shown) {
      void sync();
      return;
    }
    still = 0;
    if (frame === 0) frame = requestAnimationFrame(tick);
  }

  // Embedded: a scroll of anything holding the card moves it.
  const onScroll = (e: Event): void => {
    if (params.embedded !== true || destroyed) return;
    const target = e.target;
    if (!(target instanceof Node) || !target.contains(node)) return;
    if (params.stillWhileScrolling !== true) {
      burst();
      return;
    }
    if (scrollTimer !== null) clearTimeout(scrollTimer);
    scrollTimer = setTimeout(() => {
      scrollTimer = null;
      scrolling = false;
      burst();
    }, SCROLL_SETTLE_MS);
    if (!scrolling) {
      scrolling = true;
      void sync();
    }
  };

  // A resize (the card's, or the window's) is when a scroller's box can
  // have changed: the clipping ancestors are read again then.
  const onResize = (): void => {
    clippers = null;
    burst();
  };
  const overlays = new OverlayWatch(node, burst);
  const resizes = new ResizeObserver(onResize);
  resizes.observe(node);

  // The safety net for what nothing above reports: a pure move, or an
  // overlay that starts painting without a DOM insert (its layer un-parks
  // by flipping `inert`). One placement check, a burst only on a move. Only
  // while shown on a visible window: a parked view has nothing to follow.
  let safety: ReturnType<typeof setInterval> | null = null;
  const armSafety = (): void => {
    const want = params.shown && !destroyed && !failed;
    overlays.watch(want);
    if (want && safety === null) {
      safety = setInterval(
        () => {
          if (document.visibilityState !== "visible") return;
          if (!sameRect(measure(), lastRect)) burst();
          else void sync();
        },
        params.embedded === true ? EMBEDDED_SAFETY_MS : SAFETY_MS,
      );
    } else if (!want && safety !== null) {
      clearInterval(safety);
      safety = null;
    }
  };

  const onDragStart = (): void => {
    dragging = true;
    burst();
  };
  const onDragEnd = (): void => {
    dragging = false;
    burst();
  };
  document.addEventListener("dragstart", onDragStart, true);
  document.addEventListener("dragend", onDragEnd, true);
  document.addEventListener("drop", onDragEnd, true);
  document.addEventListener("transitionrun", burst, true);
  document.addEventListener("transitionend", burst, true);
  window.addEventListener("resize", onResize);
  document.addEventListener("scroll", onScroll, { capture: true, passive: true });
  report();
  armSafety();
  burst();

  return {
    update(next: PaneViewParams): void {
      // A new URL is a fresh load: a crashed page gives way to it.
      if (crashed && next.url !== params.url) crashed = false;
      params = next;
      report();
      const nextControl = next.control ?? null;
      if (nextControl !== control) {
        control?.unbind(bound);
        control = nextControl;
        control?.bind(bound);
      }
      armSafety();
      burst();
    },
    destroy(): void {
      destroyed = true;
      clearLoadTimer();
      unroute();
      control?.unbind(bound);
      if (frame !== 0) cancelAnimationFrame(frame);
      if (safety !== null) clearInterval(safety);
      overlays.stop();
      resizes.disconnect();
      document.removeEventListener("dragstart", onDragStart, true);
      document.removeEventListener("dragend", onDragEnd, true);
      document.removeEventListener("drop", onDragEnd, true);
      document.removeEventListener("transitionrun", burst, true);
      document.removeEventListener("transitionend", burst, true);
      window.removeEventListener("resize", onResize);
      document.removeEventListener("scroll", onScroll, { capture: true });
      if (scrollTimer !== null) clearTimeout(scrollTimer);
      if (created && !busy) void nativeViewClose(id);
    },
  };
}
