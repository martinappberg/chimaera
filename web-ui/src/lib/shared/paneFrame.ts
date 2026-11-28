/**
 * The pane-view bridge over `postMessage`, for a page in an `<iframe>` (the
 * browser, the Linux and Windows apps, and app builds without native pane
 * views): the same text-marking events and in-page pill as `paneView.ts`,
 * so a host's marking code doesn't care which transport carries its page.
 *
 * The daemon appends the bridge script to a `/raw/` HTML preview served
 * with `?bridge=1` (only the ticket's own page). The framed bridge posts
 * `{chimaeraPaneView: msg}` to this window (`msg` = the native event without
 * its `id`); the pill goes back as `{chimaeraPaneViewCommand: {op: "chip",
 * chip}}`. The frame is sandboxed (an opaque origin), so both directions use
 * `"*"` and a message is accepted only when its source is this frame's own
 * window.
 *
 * Trust: unlike the native view's isolated-world bridge, a framed page's own
 * scripts share the bridge's world and can post anything it can. So focus
 * and a selection are taken only while the user is in the frame (it holds
 * this document's focus), a reference too and at most once a second, and a
 * relayed key is honoured only as the reference chord, never replayed as a
 * workbench chord. A forged reference can at worst fill an agent's input,
 * never send it. There is no snapshot, text-under-a-rect or history here.
 */

import { REFERENCE_CHORD } from "./keys";
import type { PaneTransport, PaneViewControl } from "./paneView";
import { decodeFrameMessage, isChord } from "./paneViewBridge";
import { ownInputAgoMs } from "./ownInput";
import type { ViewRect } from "../net/native";

export interface PaneFrameParams {
  /** A press or focus inside the frame: focus its pane. */
  onFocus?: () => void;
  /** The page's text selection changed (`text` "" = cleared). */
  onSelection?: (sel: { text: string; context: string | null; rect: ViewRect | null }) => void;
  /** The in-page pill was clicked, or the reference chord pressed in it. */
  onReference?: () => void;
  /** The page loaded, or its title or same-document URL changed. */
  onPage?: (page: { url: string; title: string }) => void;
  /** The page API's `send` / `draft`: a send only on fresh evidence of the
   *  person's press, else a draft (see `pageMessageTrust`). */
  onPageMessage?: (m: { send: boolean; text: string; title: string }) => void;
  /** The page API's content height (CSS px; the page's word). */
  onPageSize?: (height: number) => void;
  /** The pointer came over the page, or left it. */
  onHover?: (inside: boolean) => void;
  /** The page wears the app's theme (see `page_themed`). */
  onPageThemed?: () => void;
  /** The app's theme, encoded (`pageTheme.ts`): posted to the page API
   *  whenever it changes and to each new document, so a framed page
   *  follows the theme without a reload. */
  theme?: string;
  control?: PaneViewControl;
}

/** The least time between two references a framed page may ask for. */
const REFERENCE_GAP_MS = 1000;
/** The least time between two messages a framed page may ask to send. */
const PAGE_MESSAGE_GAP_MS = 1000;

/**
 * What a framed page's `send` / `draft` may become. The page shares the
 * bridge's world and can post on a timer, and this window can't see into a
 * frame of another origin — not its input, not even the pointer over it (a
 * site-isolated frame runs in a process of its own) — and a canvas page
 * often keeps focus out of its frame (GenomeSpy's does). What this window
 * can see is its own transient activation, which a press or key in a frame
 * gives its ancestors too:
 * - a frame's activation is that activation when this window's last own
 *   press or key came before focus moved into this frame (the person's
 *   last press was in the frame: a page's button takes focus), or, for a
 *   page that keeps focus out (a canvas), when this window saw no press or
 *   key of its own for an activation's whole life (else it may be the
 *   app's: a click or an Enter in the composer);
 * - a send needs a frame's activation, at most one send per activation.
 *   Where the browser can't say (no `navigator.userActivation`), a page
 *   never sends;
 * - a draft needs a frame's activation, or focus in the frame;
 * - else nothing.
 * The window's activation doesn't say WHICH frame was pressed: a press in
 * another frame lets this page send once too. Bounded — one send per
 * activation and frame, and only a page the agent wrote in that chat sends
 * at all (`shared/pageMessage.ts`) — and accepted. The host then lets only a page in a chat's own answer,
 *   written while that chat ran, send (`shared/pageMessage.ts`).
 */
function pageMessageTrust(
  inFrame: boolean,
  focusedAfterOwnInput: boolean,
  ownInputAgoMs: number,
  lastSendAgoMs: number,
): "send" | "draft" | null {
  const activation = (navigator as Navigator & { userActivation?: { isActive: boolean } }).userActivation;
  const frames =
    activation !== undefined &&
    activation.isActive &&
    ((inFrame && focusedAfterOwnInput) || ownInputAgoMs > ACTIVATION_LIFE_MS);
  if (frames && lastSendAgoMs > ACTIVATION_LIFE_MS) return "send";
  return frames || inFrame ? "draft" : null;
}

/** The longest a browser keeps transient activation (Chrome and Firefox:
 *  about 5 s; WebKit: shorter). */
const ACTIVATION_LIFE_MS = 5000;

/** `use:paneFrame={{ … }}` on the `<iframe>`. */
export function paneFrame(node: HTMLIFrameElement, initial: PaneFrameParams) {
  let params = initial;
  let control = initial.control ?? null;
  let lastReference = Number.NEGATIVE_INFINITY;
  let lastPageMessage = Number.NEGATIVE_INFINITY;
  let lastPageSend = Number.NEGATIVE_INFINITY;
  let theme = initial.theme ?? "";

  const post = (cmd: Record<string, unknown>): void => {
    try {
      node.contentWindow?.postMessage({ chimaeraPaneViewCommand: cmd }, "*");
    } catch {
      /* the frame is between documents: the next page event re-sends */
    }
  };

  /** The user is in the frame: it holds this document's focus. */
  const userInFrame = (): boolean => document.activeElement === node;
  // When focus last moved into this frame (this window loses it to the
  // frame: `blur`): later than this window's own last press, it was the
  // person pressing in the frame.
  let focusedAt = Number.NEGATIVE_INFINITY;
  const onWindowBlur = (): void => {
    // The frame takes focus a tick after the window lets it go.
    setTimeout(() => {
      if (document.activeElement === node) focusedAt = performance.now();
    }, 0);
  };
  window.addEventListener("blur", onWindowBlur);


  const sendTheme = (): void => {
    if (theme !== "") post({ op: "theme", theme });
  };

  const reference = (): void => {
    if (!userInFrame()) return;
    const now = performance.now();
    if (now - lastReference < REFERENCE_GAP_MS) return;
    lastReference = now;
    params.onReference?.();
  };

  const onMessage = (e: MessageEvent): void => {
    if (e.source === null || e.source !== node.contentWindow) return;
    const ev = decodeFrameMessage(e.data);
    if (ev === null) return;
    switch (ev.kind) {
      case "focus":
        // Only when the user is in the frame: a page could otherwise post
        // `focus` on a timer and keep pulling focus to its pane.
        if (userInFrame()) params.onFocus?.();
        break;
      case "key":
        if (isChord(ev, REFERENCE_CHORD)) reference();
        break;
      case "selection":
        // A clear is always believed (it can only withdraw); a selection
        // only while the user is in the frame.
        if (ev.text.trim() === "" || userInFrame()) {
          params.onSelection?.({ text: ev.text, context: ev.context, rect: ev.rect });
        }
        break;
      case "reference":
        reference();
        break;
      case "page_size":
        params.onPageSize?.(ev.height);
        break;
      case "hover":
        params.onHover?.(ev.inside);
        break;
      case "page_themed":
        params.onPageThemed?.();
        break;
      case "page_send":
      case "page_draft": {
        const now = performance.now();
        if (now - lastPageMessage < PAGE_MESSAGE_GAP_MS) break;
        const ownAgo = ownInputAgoMs();
        const trust = pageMessageTrust(userInFrame(), now - focusedAt < ownAgo, ownAgo, now - lastPageSend);
        if (trust === null) break;
        lastPageMessage = now;
        const send = ev.kind === "page_send" && trust === "send";
        if (send) lastPageSend = now;
        params.onPageMessage?.({ send, text: ev.text, title: ev.title });
        break;
      }
      case "page":
        if (ev.url === "about:blank") break;
        // A new document starts without the pill or the newest theme
        // (its URL holds the one it was first loaded with): give both back.
        control?.applyChip();
        sendTheme();
        params.onPage?.({ url: ev.url, title: ev.title });
        break;
    }
  };
  window.addEventListener("message", onMessage);

  const transport: PaneTransport = {
    live: () => node.isConnected && node.contentWindow !== null,
    snapshot: () => Promise.resolve(null),
    chip: (chip) => post({ op: "chip", chip }),
    region: () => Promise.resolve(null),
    nav: () => {},
    sync: () => {},
  };
  control?.bind(transport);

  return {
    update(next: PaneFrameParams): void {
      params = next;
      if ((next.theme ?? "") !== theme) {
        theme = next.theme ?? "";
        sendTheme();
      }
      const nextControl = next.control ?? null;
      if (nextControl !== control) {
        control?.unbind(transport);
        control = nextControl;
        control?.bind(transport);
      }
    },
    destroy(): void {
      window.removeEventListener("message", onMessage);
      window.removeEventListener("blur", onWindowBlur);
      control?.unbind(transport);
    },
  };
}
