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
 * - **Adoption.** A host can take on a view that already exists in this
 *   window (`adopt`: an agent's own page, framed off-screen by
 *   `browser/agentBrowser.ts`, that the agent handed to the user). The
 *   action uses that id and shows the view once more at the URL it was last
 *   shown at — the shell navigates only on a changed URL, so the live page
 *   carries on (no reload), and a show without `agent` hands the view to
 *   the person. Whatever the host's own `url` is then counts as shown. From
 *   then on it owns the view like one it made: hidden while parked, closed
 *   on destroy.
 *
 * `onFallback` fires when the shell can't show the view (outside the app, an
 * app build without pane views, a refused URL): the caller renders its
 * iframe instead.
 */

import {
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
  /** Take on this existing view (read once, at mount): `id`, and the URL
   *  this window last showed it at. */
  adopt?: { id: string; url: string };
  /** The shell's id for this view once it exists (and null once it went):
   *  what the agent browser addresses a shared pane by. */
  onViewId?: (id: string | null) => void;
  /** The person acted in the page while input was being injected by an
   *  agent (the shell's `user_input`). */
  onUserInput?: () => void;
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

/** Receive the `native-view` events of view `id` (the last route set for an
 *  id wins). Returns the unroute, which removes only its own route. */
export function routeNativeView(id: string, fn: (ev: NativeViewEvent) => void): () => void {
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
  if (el.closest("[inert]") !== null) return false;
  const check = (el as Element & { checkVisibility?: (o: object) => boolean }).checkVisibility;
  if (check !== undefined) return check.call(el, { opacityProperty: true, visibilityProperty: true });
  const style = getComputedStyle(el);
  return style.display !== "none" && style.visibility !== "hidden" && style.opacity !== "0";
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
      this.observer.observe(document.body, { childList: true, subtree: true });
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
  const adopt = initial.adopt ?? null;
  const id = adopt?.id ?? `v${(seq += 1)}-${Math.random().toString(36).slice(2, 8)}`;
  let params = initial;
  /** An adopted view exists already; it waits to be shown here once. */
  let created = adopt !== null;
  let adopting = adopt !== null;
  let failed = false;
  let destroyed = false;
  /** What the shell shows now: a rect, or null (hidden). */
  let placed: ViewRect | null = null;
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

  const loadingNow = (): boolean =>
    !failed && !crashed && params.url !== null && (awaiting || (!adopting && showing !== params.url));
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
      case "user_input":
        params.onUserInput?.();
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

  const unroute = routeNativeView(id, onEvent);
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

  /** Where the view should be right now: null = hidden. */
  const wanted = (): ViewRect | null => {
    if (!params.shown || params.url === null || dragging || awaiting || crashed || control?.frozen === true) {
      return null;
    }
    const rect = measure();
    if (rect === null || overlays.covering(rect)) return null;
    return rect;
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
        if (adopting && adopt !== null && params.url !== null && params.shown && !dragging) {
          const at = wanted() ?? measure();
          if (at === null) continue;
          adopting = false;
          // Its last URL again (no navigation: a reload would lose the very
          // state that was handed over), without `agent`: the person's now.
          // Hidden like a create; the next pass places it.
          const ok = await nativeViewShow(id, adopt.url, at, true, false);
          if (destroyed) break;
          if (!ok) {
            failed = true;
            params.onFallback?.();
            return;
          }
          showing = params.url;
          placed = null;
          report();
          params.onViewId?.(id);
          control?.applyChip();
          again = true;
          continue;
        }
        const rect = wanted();
        const url = params.url;
        if (!crashed && params.shown && !dragging && url !== null && (!created || url !== showing)) {
          // A create or a new URL: the shell makes or points the view
          // hidden, and it stays so until the page reports in (set before
          // the call: a fast page can report before the call returns).
          const at = rect ?? measure();
          if (at === null) continue;
          awaitPage(created ? showing : null);
          const ok = await nativeViewShow(id, url, at, true);
          // The view exists from here even if this action is gone: the
          // `finally` closes it then.
          if (ok) {
            created = true;
            if (!destroyed) params.onViewId?.(id);
          }
          if (destroyed) break;
          if (!ok) {
            failed = true;
            awaiting = false;
            clearLoadTimer();
            params.onFallback?.();
            return;
          }
          showing = url;
          placed = null;
          report();
          control?.applyChip();
          // The page beat the call: place it on the next pass.
          if (!awaiting) again = true;
        } else if (created && !(rect === null && placed === null) && !sameRect(rect, placed)) {
          placed = rect;
          await nativeViewPlace(id, rect);
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

  const overlays = new OverlayWatch(node, burst);
  const resizes = new ResizeObserver(burst);
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
      safety = setInterval(() => {
        if (document.visibilityState !== "visible") return;
        if (!sameRect(measure(), lastRect)) burst();
        else void sync();
      }, SAFETY_MS);
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
  window.addEventListener("resize", burst);
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
      if (created && !adopting) params.onViewId?.(null);
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
      window.removeEventListener("resize", burst);
      if (created && !busy) void nativeViewClose(id);
    },
  };
}
