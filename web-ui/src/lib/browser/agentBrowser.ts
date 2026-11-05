/**
 * The agent browser, window half (#273). The daemon's MCP browser tools act
 * on a page through a window of the native app: it sends a `browser_request`
 * on `/ws/events`, this window carries it out on a pane view (the shell's
 * `native_view_*` commands) and answers with a `browser_reply` on the same
 * socket. Three kinds of page:
 *
 * - **An agent's own page**: a pane view this window frames just past its
 *   right edge (`offscreenRect`), so WebKit counts it visible and runs it at
 *   full speed while nobody sees it. Addressed by its view id (`view`).
 * - **A shared pane**: a browser tab the user linked to an agent (the chain
 *   control, like a terminal). Addressed by its tab id (`tab`); the window
 *   resolves the tab's own pane-view id through `PaneViewRegistry`.
 * - **A handed-over page**: an agent's own page the agent showed the user
 *   (`handover`). It opens as a browser tab that ADOPTS the live view (same
 *   process, no reload) and is shared with that agent from then on.
 *
 * Take-over: while an agent drives a shared pane, trusted input from the
 * person (the shell's `user_input` view event) pauses the agent
 * (`browser_event takeover`); the pane's Resume hands control back.
 *
 * Everything here is free of the DOM and the shell: `AgentBrowserChannel`
 * takes both through `AgentBrowserHost`, so the request mapping, the
 * registry, the hello diff and the take-over gate are unit-tested.
 * `agentBrowserRuntime.ts` wires the real ones. Shapes: the daemon's
 * `ws/frames.rs` (`browser_request`) and `ws.rs` `ClientMessage`.
 */

import { writable, type Readable } from "svelte/store";
import type { NativeViewNav, ViewRect } from "../net/native";
import type { NativeViewEvent } from "../shared/paneViewBridge";
import { parseAgentBrowserOpen } from "./agentOpen";
import type { BrowserEventMsg, BrowserReply, BrowserRequest } from "../net/browserWire";

export {
  decodeBrowserLinks,
  decodeBrowserRequest,
  sameHello,
  type BrowserEventMsg,
  type BrowserHello,
  type BrowserLink,
  type BrowserReply,
  type BrowserRequest,
} from "../net/browserWire";

/** How long after an agent's last action on a shared pane the person's own
 *  input still counts as taking over (beyond the daemon's `driving` flag). */
export const TAKEOVER_WINDOW_MS = 15_000;
/** How far past the window's right edge an agent's own page is framed. */
export const OFFSCREEN_GAP_PX = 64;

const isObj = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null;

/** The view id the shell probe drives: valid, and never a real view. */
export const PROBE_VIEW_ID = "probe-none";

/**
 * Whether a probe of `native_view_drive` on a view that doesn't exist
 * shows the command is there: the command answering at all (with "no such
 * view", "bad view request", or a result) says yes; the IPC layer refusing
 * the command name (an older app build: "not found", "not allowed" by its
 * permissions) says no. Only an app that can drive pages claims the app
 * engine; otherwise the daemon falls back to Chromium.
 */
export function driveProbeSaysPresent(outcome: { ok: true } | { ok: false; error: unknown }): boolean {
  if (outcome.ok) return true;
  const e = outcome.error;
  const text = e instanceof Error ? e.message : typeof e === "string" ? e : "";
  if (text === "") return false;
  return !/not found|not allowed|denied|permission|unknown command|\bacl\b|not in the app/i.test(text);
}

/** `hsl(hue 65% 55%)` (the agent's link hue, as the chips paint it) as
 *  `#rrggbb`, the shell's cursor colour format. */
export function hueHex(hue: number): string {
  const s = 0.65;
  const l = 0.55;
  const a = s * Math.min(l, 1 - l);
  const channel = (n: number): string => {
    const k = (n + hue / 30) % 12;
    const v = l - a * Math.max(-1, Math.min(k - 3, 9 - k, 1));
    return Math.round(v * 255)
      .toString(16)
      .padStart(2, "0");
  };
  return `#${channel(0)}${channel(8)}${channel(4)}`;
}

/** Where an agent's own page sits: just past the window's right edge, at
 *  the size the agent asked for. */
export function offscreenRect(viewportWidth: number, width: number, height: number): ViewRect {
  return { x: Math.round(viewportWidth) + OFFSCREEN_GAP_PX, y: 0, width, height };
}

// --- the tab → view registry ------------------------------------------------

/**
 * Browser tab id → the id of its live pane view, as mounted in THIS window.
 * A `BrowserView` registers once its view exists and unregisters when it
 * goes; a handed-over tab counts from the hand-over until its view adopts
 * the page (it may sit unmounted behind a zoomed pane meanwhile).
 */
export class PaneViewRegistry {
  private readonly byTab = new Map<string, string>();
  private readonly adopting = new Map<string, string>();
  private readonly subs = new Set<() => void>();

  register(tab: string, view: string): void {
    const had = this.adopting.delete(tab);
    if (this.byTab.get(tab) === view && !had) return;
    this.byTab.set(tab, view);
    this.changed();
  }

  /** Only the registration it made: a remount that registered first keeps
   *  the tab. */
  unregister(tab: string, view: string): void {
    if (this.byTab.get(tab) !== view) return;
    this.byTab.delete(tab);
    this.changed();
  }

  /** `tab` was opened to adopt `view` (a hand-over). */
  expectAdoption(tab: string, view: string): void {
    this.adopting.set(tab, view);
    this.changed();
  }

  /** The hand-over will not complete (its tab went before adopting). */
  dropAdoption(tab: string): void {
    if (this.adopting.delete(tab)) this.changed();
  }

  /** The view mounted for `tab` in this window (not a pending adoption). */
  mountedView(tab: string): string | null {
    return this.byTab.get(tab) ?? null;
  }

  /** Every hand-over still waiting for its view: [tab, view]. */
  pendingAdoptions(): [string, string][] {
    return [...this.adopting];
  }

  /** The adoption `tab` still waits for, if any. */
  pendingAdoption(tab: string): string | null {
    return this.adopting.get(tab) ?? null;
  }

  viewFor(tab: string): string | null {
    return this.byTab.get(tab) ?? this.adopting.get(tab) ?? null;
  }

  tabFor(view: string): string | null {
    for (const [tab, v] of this.byTab) if (v === view) return tab;
    for (const [tab, v] of this.adopting) if (v === view) return tab;
    return null;
  }

  /** Every tab with a view here, sorted (the hello's `tabs`). */
  tabs(): string[] {
    return [...new Set([...this.byTab.keys(), ...this.adopting.keys()])].sort();
  }

  onChange(fn: () => void): () => void {
    this.subs.add(fn);
    return () => this.subs.delete(fn);
  }

  private changed(): void {
    for (const fn of this.subs) fn();
  }
}

// --- drive state and the take-over gate --------------------------------------

/** What a shared pane's chrome shows about its agent (the daemon's `state`
 *  op, plus when the agent last acted here). */
export interface DriveState {
  driving: boolean;
  paused: boolean;
  /** `now()` of the agent's last request on this tab, or null. */
  lastDriveAt: number | null;
}

/**
 * Does the person's input on a pane take it over from its agent? Only on a
 * shared pane, not already paused, that the agent is driving now or acted
 * on within `TAKEOVER_WINDOW_MS` — clicking around a pane the agent left
 * alone pauses nothing.
 */
export function shouldTakeOver(shared: boolean, state: DriveState | undefined, now: number): boolean {
  if (!shared || state === undefined || state.paused) return false;
  if (state.driving) return true;
  return state.lastDriveAt !== null && now - state.lastDriveAt <= TAKEOVER_WINDOW_MS;
}

// --- the request handler -------------------------------------------------------

/** The shell calls the handler makes (`net/native.ts` in the app). */
export interface AgentBrowserNative {
  show(id: string, url: string, rect: ViewRect, hidden: boolean, agent: boolean): Promise<boolean>;
  /** Rejects with the shell's message. */
  drive(id: string, action: unknown): Promise<unknown>;
  nav(id: string, action: NativeViewNav): Promise<void>;
  close(id: string): Promise<void>;
  place(id: string, rect: ViewRect | null): Promise<void>;
}

/** An agent's own page, to open as a pane adopting its view. */
export interface Handover {
  view: string;
  host: string;
  port: number;
  /** Normalized like a clicked URL's path. */
  path: string;
  /** The agent session it is handed over by. */
  agent: string;
}

export interface AgentBrowserHost {
  native: AgentBrowserNative;
  /** A client message on `/ws/events`; false when the socket is not open. */
  send(msg: BrowserReply | BrowserEventMsg): boolean;
  /** The window's width in CSS px (where "off-screen" starts). */
  viewportWidth(): number;
  /** Open the pane for a hand-over: its tab id, or throws an Error whose
   *  message says why not. */
  handover(h: Handover): string;
  /** Whether `tab` is shared with an agent (the mirrored browser links). */
  shared(tab: string): boolean;
  /** Receive a view's events (`shared/paneView.ts` `routeNativeView`). */
  route(view: string, fn: (ev: NativeViewEvent) => void): () => void;
  now(): number;
  /** Send this window's `browser_hello` now, past its debounce (a
   *  hand-over's new tab must be known before the agent's next call). */
  helloNow(): void;
  /** The agent cursor's colour on a shared tab (`#rrggbb`, the hue of the
   *  agent it is shared with), or null. */
  cursorColor(tab: string): string | null;
}

const NAV_ACTIONS: readonly NativeViewNav[] = ["back", "forward", "reload", "stop"];
/** Bounds on an own page's size: the shell grows a view for a full-page
 *  screenshot itself, so anything past a large monitor is a bad request. */
const MIN_VIEW_PX = 100;
const MAX_VIEW_PX = 8000;

/** A request this window can't carry out; the message reaches the agent. */
class Refusal extends Error {}

function message(e: unknown): string {
  if (e instanceof Error && e.message !== "") return e.message;
  if (typeof e === "string" && e !== "") return e;
  return "the window could not do that";
}

function size(v: unknown, what: string): number {
  if (typeof v !== "number" || !Number.isFinite(v)) throw new Refusal(`missing ${what}`);
  return Math.min(MAX_VIEW_PX, Math.max(MIN_VIEW_PX, Math.round(v)));
}

/** An agent's own view this window frames. `handedTo` is the tab opened to
 *  adopt it: it stays framed off-screen (and survives a socket drop) until
 *  that tab's view takes it on. */
interface OwnView {
  /** The URL this window last SHOWED it at — not where the page is now:
   *  the agent navigates in the page (`location.assign`), which the shell
   *  never hears of, and the shell navigates a re-shown view only when the
   *  URL differs from the one it was last shown at. So an adopting pane
   *  shows it at this URL to keep the live page, wherever it went since. */
  url: string;
  width: number;
  height: number;
  handedTo: string | null;
  unroute: () => void;
}

/**
 * One per window. `handle` answers every request exactly once (a reply the
 * closed socket can't carry is dropped: the daemon already failed it).
 */
export class AgentBrowserChannel {
  readonly registry = new PaneViewRegistry();
  private readonly own = new Map<string, OwnView>();
  private readonly driveStore = writable<ReadonlyMap<string, DriveState>>(new Map());
  private drive = new Map<string, DriveState>();
  /** Per-tab drive state for the pane chrome (keyed by BrowserTab id). */
  readonly driveState: Readable<ReadonlyMap<string, DriveState>> = { subscribe: this.driveStore.subscribe };

  constructor(private readonly host: AgentBrowserHost) {}

  async handle(req: BrowserRequest): Promise<void> {
    let result: unknown;
    try {
      result = await this.run(req);
    } catch (e) {
      this.host.send({ type: "browser_reply", rid: req.rid, ok: false, error: message(e) });
      return;
    }
    this.host.send({ type: "browser_reply", rid: req.rid, ok: true, result: result ?? {} });
  }

  private async run(req: BrowserRequest): Promise<unknown> {
    const f = req.fields;
    switch (req.op) {
      case "open": {
        const view = this.ownId(req);
        const url = f.url;
        // Same-origin only (`/proxy/{id}…` minted by the daemon): never an
        // absolute URL the page view would leave this daemon for.
        if (typeof url !== "string" || !url.startsWith("/") || url.startsWith("//")) {
          throw new Refusal("missing or foreign page url");
        }
        const width = size(f.width, "width");
        const height = size(f.height, "height");
        const ok = await this.host.native.show(
          view,
          url,
          offscreenRect(this.host.viewportWidth(), width, height),
          false,
          true,
        );
        if (!ok) throw new Refusal("this window can't host an agent page (pane views need the macOS app)");
        const known = this.own.get(view);
        if (known !== undefined) {
          known.url = url;
          known.width = width;
          known.height = height;
        } else {
          this.own.set(view, {
            url,
            width,
            height,
            handedTo: null,
            unroute: this.host.route(view, (ev) => {
              if (ev.kind === "crashed") this.lost(view);
            }),
          });
        }
        return {};
      }
      case "drive": {
        const { view, tab } = this.resolve(req);
        if (!isObj(f.action)) throw new Refusal("missing action");
        let action: Record<string, unknown> = f.action;
        if (tab !== null) {
          this.noteDrive(tab);
          // The cursor drawn over a shared pane wears its agent's hue, the
          // colour of the pane's border and chips.
          if (action.type === "cursor" && action.color === undefined) {
            const color = this.host.cursorColor(tab);
            if (color !== null) action = { ...action, color };
          }
        }
        return await this.host.native.drive(view, action);
      }
      case "nav": {
        const { view, tab } = this.resolve(req);
        const action = f.action;
        if (typeof action !== "string" || !NAV_ACTIONS.includes(action as NativeViewNav)) {
          throw new Refusal("nav takes back, forward, reload or stop");
        }
        if (tab !== null) this.noteDrive(tab);
        await this.host.native.nav(view, action as NativeViewNav);
        return {};
      }
      case "close": {
        if (req.view === null) throw new Refusal("a shared pane belongs to the user and stays open");
        const own = this.own.get(req.view);
        if (own === undefined && this.registry.tabFor(req.view) !== null) {
          throw new Refusal("that page is a pane now and belongs to the user");
        }
        this.forget(req.view);
        await this.host.native.close(req.view);
        return {};
      }
      case "handover": {
        const view = this.ownId(req);
        const own = this.own.get(view);
        if (own === undefined) throw new Refusal("that page is not open in this window any more");
        if (own.handedTo !== null) return { tab: own.handedTo };
        // The pane's own validation, as for an `open_browser` frame.
        const open = parseAgentBrowserOpen({
          session_id: f.agent,
          host: f.host,
          port: f.port,
          path: f.path,
        });
        if (open === null) throw new Refusal("missing or malformed page address");
        const tab = this.host.handover({ view, host: open.host, port: open.port, path: open.path, agent: open.sessionId });
        own.handedTo = tab;
        this.registry.expectAdoption(tab, view);
        // Before the reply: the daemon must know the tab is here by the time
        // the agent's next call addresses it.
        this.host.helloNow();
        return { tab };
      }
      case "state": {
        const tab = req.tab ?? (typeof f.tab === "string" && f.tab !== "" ? f.tab : null);
        if (tab === null) throw new Refusal("missing tab");
        if (typeof f.driving !== "boolean" || typeof f.paused !== "boolean") {
          throw new Refusal("missing driving or paused");
        }
        const prev = this.drive.get(tab);
        this.setDrive(tab, {
          driving: f.driving,
          paused: f.paused,
          lastDriveAt: f.driving ? this.host.now() : (prev?.lastDriveAt ?? null),
        });
        return {};
      }
      default:
        throw new Refusal(`this window doesn't know "${req.op}" (an older build: reload it)`);
    }
  }

  /** The person acted in `tab`'s page (`user_input`): take it over when it
   *  is shared and its agent is at it. True when the take-over was sent. */
  userInput(tab: string): boolean {
    if (!shouldTakeOver(this.host.shared(tab), this.drive.get(tab), this.host.now())) return false;
    if (!this.host.send({ type: "browser_event", kind: "takeover", view: this.registry.viewFor(tab), tab })) return false;
    // Paused at once: the next keystroke must not send another, and the
    // chrome answers now. The daemon's `state` confirms (or corrects) it.
    this.setDrive(tab, { driving: false, paused: true, lastDriveAt: this.drive.get(tab)?.lastDriveAt ?? null });
    return true;
  }

  /** The pane's Resume: hand `tab` back to its agent. */
  resume(tab: string): boolean {
    if (!this.host.send({ type: "browser_event", kind: "resume", view: this.registry.viewFor(tab), tab })) return false;
    const prev = this.drive.get(tab);
    this.setDrive(tab, { driving: false, paused: false, lastDriveAt: prev?.lastDriveAt ?? null });
    return true;
  }

  /** What a hand-over tab's view adopts: the own view `view` and the URL
   *  this window last showed it at; null once it is gone. */
  adoption(view: string): { id: string; url: string } | null {
    const own = this.own.get(view);
    return own !== undefined && own.handedTo !== null ? { id: view, url: own.url } : null;
  }

  /** A tab's view took on the page it was opened to adopt. */
  adopted(tab: string, view: string): void {
    const own = this.own.get(view);
    if (own !== undefined) {
      own.unroute();
      this.own.delete(view);
    }
    this.registry.register(tab, view);
  }

  /** A hand-over tab went before its view adopted the page: the page has no
   *  owner left, so it closes. */
  abandonAdoption(tab: string, view: string): void {
    if (this.registry.pendingAdoption(tab) !== view) return;
    this.registry.dropAdoption(tab);
    this.forget(view);
    void this.host.native.close(view);
  }

  /** The window resized: keep every own page just past the new edge. */
  relayout(): void {
    const w = this.host.viewportWidth();
    for (const [view, own] of this.own) {
      void this.host.native.place(view, offscreenRect(w, own.width, own.height));
    }
  }

  /**
   * The events socket went down: the daemon has failed every page this
   * window served, so the agents' own pages close (nothing can reach them
   * again) and no pane is driven any more. A handed-over page waiting for
   * its pane stays: it is the user's now.
   */
  socketDown(): void {
    for (const [view, own] of [...this.own]) {
      if (own.handedTo !== null) continue;
      this.forget(view);
      void this.host.native.close(view);
    }
    if (this.drive.size > 0) {
      this.drive = new Map();
      this.driveStore.set(this.drive);
    }
  }

  /** The agent no longer holds `tab` (unlinked): its marker goes, and its
   *  view, if this window holds it, stops recording for the agent in its
   *  future loads (best effort). */
  forgetTab(tab: string): void {
    const view = this.registry.mountedView(tab);
    if (view !== null) this.host.native.drive(view, { type: "uninstrument" }).catch(() => {});
    if (!this.drive.has(tab)) return;
    this.drive = new Map(this.drive);
    this.drive.delete(tab);
    this.driveStore.set(this.drive);
  }

  private ownId(req: BrowserRequest): string {
    if (req.view === null) throw new Refusal("missing view");
    return req.view;
  }

  private resolve(req: BrowserRequest): { view: string; tab: string | null } {
    if (req.view !== null) {
      // Only an agent page this window opened: never another pane view of
      // the window (an HTML preview, a browser tab nobody shared).
      if (!this.own.has(req.view)) throw new Refusal("that page is not an agent page of this window");
      return { view: req.view, tab: req.tab ?? this.registry.tabFor(req.view) };
    }
    if (req.tab === null) throw new Refusal("missing view or tab");
    const view = this.registry.viewFor(req.tab);
    if (view === null) throw new Refusal("that pane isn't open in this window any more");
    return { view, tab: req.tab };
  }

  private noteDrive(tab: string): void {
    const prev = this.drive.get(tab);
    this.setDrive(tab, { driving: prev?.driving ?? false, paused: prev?.paused ?? false, lastDriveAt: this.host.now() });
  }

  private setDrive(tab: string, next: DriveState): void {
    const prev = this.drive.get(tab);
    if (
      prev !== undefined &&
      prev.driving === next.driving &&
      prev.paused === next.paused &&
      prev.lastDriveAt === next.lastDriveAt
    ) {
      return;
    }
    this.drive = new Map(this.drive);
    this.drive.set(tab, next);
    // The chrome reads only driving/paused: a bare timestamp bump on a
    // busy agent must not re-render every pane bar.
    if (prev === undefined || prev.driving !== next.driving || prev.paused !== next.paused) {
      this.driveStore.set(this.drive);
    }
  }

  private forget(view: string): void {
    const own = this.own.get(view);
    if (own === undefined) return;
    own.unroute();
    this.own.delete(view);
  }

  /** An own page's process died for good: tell the daemon, drop it. */
  private lost(view: string): void {
    const own = this.own.get(view);
    if (own === undefined) return;
    const tab = own.handedTo;
    this.forget(view);
    if (tab !== null) this.registry.dropAdoption(tab);
    void this.host.native.close(view);
    this.host.send({ type: "browser_event", kind: "view_lost", view, tab });
  }
}
