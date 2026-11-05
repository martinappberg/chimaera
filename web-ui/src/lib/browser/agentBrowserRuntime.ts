/**
 * The agent browser's window wiring: one `AgentBrowserChannel`
 * (`agentBrowser.ts`) per window on the real shell calls, the mirrored
 * browser links, and the `browser_hello` this window keeps the daemon
 * told of. App attaches it to its `/ws/events` socket once; `BrowserView`
 * registers its pane view and reports the person's input through it.
 */

import { get, writable, type Readable } from "svelte/store";
import { nativeViewClose, nativeViewDrive, nativeViewNav, nativeViewPlace, nativeViewShow } from "../net/native";
import { paneViewsPossible, routeNativeView } from "../shared/paneView";
import type { BrowserClientMessage } from "../net/browserWire";
import { agentHue } from "../workspace/agentLinks";
import { AgentBrowserChannel, PROBE_VIEW_ID, driveProbeSaysPresent, hueHex, sameHello, type BrowserHello, type BrowserLink, type Handover } from "./agentBrowser";

/** What the runtime needs of the events socket (`net/events.ts`). */
export interface AgentBrowserSocket {
  send(msg: BrowserClientMessage): boolean;
  /** Kept and re-sent after every (re)auth. */
  setHello(msg: BrowserClientMessage | null): void;
}

let socket: AgentBrowserSocket | null = null;
let openPane: ((h: Handover) => string) | null = null;

const links = writable<ReadonlyMap<string, string>>(new Map());
/** Browser tab id → the agent it is shared with (the sessions frame's
 *  `browser_links`, mirrored; optimistic between a PUT and the next frame). */
export const browserLinks: Readable<ReadonlyMap<string, string>> = { subscribe: links.subscribe };

export const agentBrowser = new AgentBrowserChannel({
  native: {
    show: async (id, url, rect, hidden, agent) => (await nativeViewShow(id, url, rect, hidden, { agent })) !== null,
    drive: nativeViewDrive,
    nav: nativeViewNav,
    close: nativeViewClose,
    place: nativeViewPlace,
  },
  send: (msg) => socket?.send(msg) ?? false,
  viewportWidth: () => window.innerWidth,
  handover: (h) => {
    if (openPane === null) throw new Error("this window isn't ready to open a pane");
    return openPane(h);
  },
  shared: (tab) => get(links).has(tab),
  route: routeNativeView,
  now: () => Date.now(),
  helloNow: () => reportHello(),
  cursorColor: (tab) => {
    const agent = get(links).get(tab);
    return agent === undefined ? null : hueHex(agentHue(agent));
  },
});

/** Replace the mirrored links (a sessions frame). A tab no longer shared
 *  drops its driving marker. */
export function setBrowserLinks(next: readonly BrowserLink[]): void {
  const map = new Map(next.map((l) => [l.tab, l.agent_id]));
  const prev = get(links);
  if (prev.size === map.size && [...map].every(([t, a]) => prev.get(t) === a)) return;
  for (const tab of prev.keys()) if (!map.has(tab)) agentBrowser.forgetTab(tab);
  links.set(map);
}

/** One link changed here, before the daemon's frame confirms it (`null`
 *  agent = unlinked). */
export function setBrowserLinkLocally(tab: string, agent: string | null): void {
  const map = new Map(get(links));
  if (agent === null) {
    if (!map.delete(tab)) return;
    agentBrowser.forgetTab(tab);
  } else {
    if (map.get(tab) === agent) return;
    map.set(tab, agent);
  }
  links.set(map);
}

// --- browser_hello ------------------------------------------------------------

/** Coalesces a burst (a window drag-resize, focus flapping between panes'
 *  own web processes) into one hello. */
const HELLO_DEBOUNCE_MS = 200;

let lastHello: BrowserHello | null = null;

/** Whether this app build can drive pages (`native_view_drive`), probed
 *  once per page load; false until the probe says so. */
let canDrive = false;
let driveProbe: Promise<void> | null = null;

function probeDrive(): Promise<void> {
  driveProbe ??= paneViewsPossible()
    ? nativeViewDrive(PROBE_VIEW_ID, { type: "rings", console_since: 0, network_since: 0 }).then(
        () => ({ ok: true }) as const,
        (error: unknown) => ({ ok: false, error }) as const,
      ).then((outcome) => {
        canDrive = driveProbeSaysPresent(outcome);
      })
    : Promise.resolve();
  return driveProbe;
}
let helloTimer: ReturnType<typeof setTimeout> | null = null;

function currentHello(): BrowserHello {
  return {
    type: "browser_hello",
    app_engine: canDrive && paneViewsPossible(),
    visible: document.visibilityState === "visible",
    focused: document.hasFocus(),
    width: Math.round(window.innerWidth),
    height: Math.round(window.innerHeight),
    tabs: agentBrowser.registry.tabs(),
  };
}

function reportHello(): void {
  if (helloTimer !== null) clearTimeout(helloTimer);
  helloTimer = null;
  const hello = currentHello();
  if (sameHello(lastHello, hello)) return;
  lastHello = hello;
  socket?.setHello(hello);
}

function scheduleHello(): void {
  if (helloTimer !== null) clearTimeout(helloTimer);
  helloTimer = setTimeout(reportHello, HELLO_DEBOUNCE_MS);
}

/**
 * Serve the agent browser on `s` (App's events socket). `handover` opens the
 * pane for a handed-over page and returns its tab id (it throws to refuse).
 * Returns the detach.
 */
export function attachAgentBrowser(s: AgentBrowserSocket, handover: (h: Handover) => string): () => void {
  socket = s;
  openPane = handover;
  lastHello = null;
  reportHello();
  // An app build with pane views but no driving claims nothing until the
  // probe answers; then the hello says what it can do.
  void probeDrive().then(() => {
    if (socket === s) reportHello();
  });
  const onResize = (): void => {
    agentBrowser.relayout();
    scheduleHello();
  };
  // Focus can land in a pane view's own process: the window's blur then
  // reads as unfocused, which is right — the daemon prefers a window the
  // person is in, and a page they are typing into is not that window.
  document.addEventListener("visibilitychange", scheduleHello);
  window.addEventListener("focus", scheduleHello);
  window.addEventListener("blur", scheduleHello);
  window.addEventListener("resize", onResize);
  const offRegistry = agentBrowser.registry.onChange(scheduleHello);
  return () => {
    document.removeEventListener("visibilitychange", scheduleHello);
    window.removeEventListener("focus", scheduleHello);
    window.removeEventListener("blur", scheduleHello);
    window.removeEventListener("resize", onResize);
    offRegistry();
    if (helloTimer !== null) clearTimeout(helloTimer);
    helloTimer = null;
    if (socket === s) {
      socket = null;
      openPane = null;
    }
  };
}
