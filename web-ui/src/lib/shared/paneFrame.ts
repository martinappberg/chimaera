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
  control?: PaneViewControl;
}

/** The least time between two references a framed page may ask for. */
const REFERENCE_GAP_MS = 1000;

/** `use:paneFrame={{ … }}` on the `<iframe>`. */
export function paneFrame(node: HTMLIFrameElement, initial: PaneFrameParams) {
  let params = initial;
  let control = initial.control ?? null;
  let lastReference = Number.NEGATIVE_INFINITY;

  const post = (cmd: Record<string, unknown>): void => {
    try {
      node.contentWindow?.postMessage({ chimaeraPaneViewCommand: cmd }, "*");
    } catch {
      /* the frame is between documents: the next page event re-sends */
    }
  };

  /** The user is in the frame: it holds this document's focus. */
  const userInFrame = (): boolean => document.activeElement === node;

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
      case "page":
        if (ev.url === "about:blank") break;
        // A new document starts without the pill: give it back.
        control?.applyChip();
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
      const nextControl = next.control ?? null;
      if (nextControl !== control) {
        control?.unbind(transport);
        control = nextControl;
        control?.bind(transport);
      }
    },
    destroy(): void {
      window.removeEventListener("message", onMessage);
      control?.unbind(transport);
    },
  };
}
