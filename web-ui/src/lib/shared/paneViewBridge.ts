/**
 * The pure half of the pane-view bridge (`paneView.ts`): decoding what a
 * native pane view reports (the `native-view` event), the synthetic chord a
 * view's ⌘/Ctrl keydown becomes, and the in-page reference pill's words.
 *
 * Event payloads are built by the shell from what a page did (its selection
 * text, its title), so the decoder is strict about shape and lenient about
 * extra fields: anything malformed is dropped, never half-applied. Shapes:
 * `crates/chimaera-app/src/shell/native_view.rs`.
 */

export interface ViewRectLike {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface NativeViewKey {
  key: string;
  code: string;
  metaKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
}

export type NativeViewEvent =
  | { id: string; kind: "focus" }
  | ({ id: string; kind: "key" } & NativeViewKey)
  | { id: string; kind: "selection"; text: string; context: string | null; rect: ViewRectLike | null }
  | { id: string; kind: "reference" }
  | { id: string; kind: "page"; url: string; title: string }
  /** The view's own web process died twice within a minute; the shell
   *  stopped reloading it. */
  | { id: string; kind: "crashed" }
  /** Trusted input in the view that matches no input an agent injected:
   *  the person at the window acted (agent browser take-over). */
  | { id: string; kind: "user_input" }
  /** The page API's `send` / `draft` (`chimaera-core` `page_api.js`): a
   *  page asking to message the agent, believed only right after the
   *  person's own input (the native bridge checks; a framed page's host
   *  checks itself). `title` is the page's own and only words. */
  | { id: string; kind: "page_send" | "page_draft"; text: string; title: string }
  /** The page API's report of the page's content height (CSS px), for an
   *  inline card that takes the page's own height. The page's word: a host
   *  clamps it. */
  | { id: string; kind: "page_size"; height: number }
  /** The pointer came over the page or left it (trusted pointer events,
   *  for a control that shows on hover). */
  | { id: string; kind: "hover"; inside: boolean }
  /** The page wears the app's theme (the page API: `--ch-*` in its own
   *  styles, `#inline`, or its meta tag): its host may load it again for a
   *  new theme. A page that doesn't is never reloaded for one. */
  | { id: string; kind: "page_themed" }
  /** web/1, a plugin's page: run its plugin's `action` (`act`). The page's
   *  word: its host checks the page is a plugin's and rate-limits it. */
  | { id: string; kind: "web_act"; action: string; payload: unknown }
  /** web/1, a plugin's page: its status pill. */
  | { id: string; kind: "web_status"; state: WebState; text: string }
  /** web/1: point the agent at something in the page, with a picture —
   *  a draft, never sent (`reference`). `crop`: the picture (a Blob from a
   *  frame, a data URL from a native view) or a rect for the host to take. */
  | {
      id: string;
      kind: "web_reference";
      text: string;
      file: string | null;
      line: number | null;
      crop: WebCrop | null;
      title: string;
    };

export type WebState = "idle" | "busy" | "ok" | "warn" | "bad";
export type WebCrop = { blob: Blob } | { dataUrl: string } | { rect: ViewRectLike };

/** web/1's page events, as either transport delivers them. */
export type WebEvent = WithoutId<Extract<NativeViewEvent, { kind: "web_act" | "web_status" | "web_reference" }>>;

const WEB_STATES: readonly string[] = ["idle", "busy", "ok", "warn", "bad"];
/** A `data:image/png` (or jpeg/webp) crop a native page may hand over. */
const CROP_DATA_URL = /^data:image\/(png|jpeg|webp);base64,[A-Za-z0-9+/=]+$/;
const CROP_DATA_MAX = 200_000;

/** The most of a page's message, and of its title, that is kept. */
export const PAGE_TEXT_MAX = 4000;
const PAGE_TITLE_MAX = 120;

const isObj = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null;
const str = (v: unknown): v is string => typeof v === "string";
const bool = (v: unknown): v is boolean => typeof v === "boolean";
const num = (v: unknown): v is number => typeof v === "number" && Number.isFinite(v);

function rectOf(v: unknown): ViewRectLike | null {
  if (!isObj(v) || !num(v.x) || !num(v.y) || !num(v.width) || !num(v.height)) return null;
  return { x: v.x, y: v.y, width: v.width, height: v.height };
}

function cropOf(v: unknown): WebCrop | null {
  if (typeof Blob !== "undefined" && v instanceof Blob) {
    return v.type.startsWith("image/") || v.type === "" ? { blob: v } : null;
  }
  if (str(v)) return v.length <= CROP_DATA_MAX && CROP_DATA_URL.test(v) ? { dataUrl: v } : null;
  const rect = rectOf(v);
  return rect !== null && rect.width > 0 && rect.height > 0 ? { rect } : null;
}

/** A `native-view` payload as a typed event, or null when it is not one. */
export function decodeNativeViewEvent(raw: unknown): NativeViewEvent | null {
  if (!isObj(raw) || !str(raw.id) || !str(raw.kind)) return null;
  const id = raw.id;
  switch (raw.kind) {
    case "focus":
      return { id, kind: "focus" };
    case "reference":
      return { id, kind: "reference" };
    case "crashed":
      return { id, kind: "crashed" };
    case "user_input":
      return { id, kind: "user_input" };
    case "key":
      if (!str(raw.key) || !str(raw.code)) return null;
      if (![raw.metaKey, raw.ctrlKey, raw.altKey, raw.shiftKey].every(bool)) return null;
      return {
        id,
        kind: "key",
        key: raw.key,
        code: raw.code,
        metaKey: raw.metaKey as boolean,
        ctrlKey: raw.ctrlKey as boolean,
        altKey: raw.altKey as boolean,
        shiftKey: raw.shiftKey as boolean,
      };
    case "selection": {
      if (!str(raw.text)) return null;
      const context = str(raw.context) && raw.context.trim() !== "" ? raw.context.trim() : null;
      return { id, kind: "selection", text: raw.text, context, rect: rectOf(raw.rect) };
    }
    case "page":
      if (!str(raw.url) || !str(raw.title)) return null;
      return { id, kind: "page", url: raw.url, title: raw.title };
    case "page_themed":
      return { id, kind: "page_themed" };
    case "hover":
      if (!bool(raw.inside)) return null;
      return { id, kind: "hover", inside: raw.inside };
    case "page_size":
      if (!num(raw.height) || raw.height < 0) return null;
      return { id, kind: "page_size", height: Math.min(raw.height, 100_000) };
    case "page_send":
    case "page_draft": {
      if (!str(raw.text) || raw.text.trim() === "") return null;
      const title = str(raw.title) ? raw.title.replace(/\s+/g, " ").trim().slice(0, PAGE_TITLE_MAX) : "";
      return { id, kind: raw.kind, text: raw.text.slice(0, PAGE_TEXT_MAX), title };
    }
    case "web_act":
      if (!str(raw.action) || raw.action === "" || raw.action.length > 64) return null;
      return { id, kind: "web_act", action: raw.action, payload: raw.payload ?? null };
    case "web_status":
      if (!str(raw.state) || !WEB_STATES.includes(raw.state)) return null;
      return { id, kind: "web_status", state: raw.state as WebState, text: str(raw.text) ? raw.text.slice(0, 200) : "" };
    case "web_reference": {
      if (!str(raw.text)) return null;
      const title = str(raw.title) ? raw.title.replace(/\s+/g, " ").trim().slice(0, PAGE_TITLE_MAX) : "";
      return {
        id,
        kind: "web_reference",
        text: raw.text.slice(0, PAGE_TEXT_MAX),
        file: str(raw.file) && raw.file !== "" ? raw.file.slice(0, 1000) : null,
        line: num(raw.line) && Number.isInteger(raw.line) && raw.line > 0 ? raw.line : null,
        crop: cropOf(raw.crop),
        title,
      };
    }

    default:
      return null;
  }
}

/**
 * The keydown a view's chord is replayed as on this window: App's chord
 * engine listens on `window` in the capture phase and matches on these
 * fields alone. Cancelable, so a handler's `preventDefault` reads as
 * "claimed".
 */
export function chordInit(k: NativeViewKey): KeyboardEventInit {
  return {
    key: k.key,
    code: k.code,
    metaKey: k.metaKey,
    ctrlKey: k.ctrlKey,
    altKey: k.altKey,
    shiftKey: k.shiftKey,
    bubbles: true,
    cancelable: true,
  };
}

/**
 * The in-page pill's words: `shared/ReferenceChip.svelte`'s, for a page the
 * bridge draws it in (that component can't reach into a view). The bridge
 * draws the `@` mark itself, before `text`. `chord` is `PINNED.reference`,
 * passed in to keep this module free of the keymap.
 */
export function referencePill(
  target: { name: string } | null,
  label: string | undefined,
  chord: string,
): { text: string; title: string; enabled: boolean } {
  return {
    text: "reference in agent",
    title:
      target === null
        ? "no agent session in this workspace — start one to reference"
        : `reference ${label !== undefined ? `${label} ` : ""}in ${target.name} (${chord})`,
    enabled: target !== null,
  };
}

/** Same pill, same words: no shell call when nothing changed. */
export function samePill(
  a: { text: string; title: string; enabled: boolean } | null,
  b: { text: string; title: string; enabled: boolean } | null,
): boolean {
  if (a === null || b === null) return a === b;
  return a.text === b.text && a.title === b.title && a.enabled === b.enabled;
}

// --- the framed transport (an iframe preview's bridge, over postMessage) -------

/** A framed page's events carry no view id: the frame is the address. */
type WithoutId<T> = T extends unknown ? Omit<T, "id"> : never;

export type FrameEvent = WithoutId<Exclude<NativeViewEvent, { kind: "crashed" | "user_input" }>>;

/**
 * A `message` from a framed page's bridge (`{chimaeraPaneView: msg}`), or
 * null when it is not one. The caller has already checked the message came
 * from its own frame; nothing here can tell the bridge from the page's own
 * scripts, which share its world.
 */
export function decodeFrameMessage(data: unknown): FrameEvent | null {
  if (!isObj(data) || !isObj(data.chimaeraPaneView)) return null;
  const m = data.chimaeraPaneView;
  // A crash and a take-over are the shell's to report, never a page's.
  if (m.kind === "crashed" || m.kind === "user_input") return null;
  // Same shapes as the native event, addressed by the frame instead of an id.
  const ev = decodeNativeViewEvent({ ...m, id: "" });
  if (ev === null) return null;
  const out: Partial<NativeViewEvent> = { ...ev };
  delete out.id;
  return out as FrameEvent;
}

/** A chord as `shared/keys.ts` parses one. */
export interface ChordLike {
  meta: boolean;
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  key: string;
}

/** Whether a relayed key is exactly `chord`. */
export function isChord(k: NativeViewKey, chord: ChordLike): boolean {
  return (
    k.metaKey === chord.meta &&
    k.ctrlKey === chord.ctrl &&
    k.altKey === chord.alt &&
    k.shiftKey === chord.shift &&
    k.key.toLowerCase() === chord.key.toLowerCase()
  );
}

/** `url` with `pair` added to its query (unless it is already there), any
 *  query and fragment it carries kept. */
function withPair(url: string, pair: string): string {
  const hash = url.indexOf("#");
  const base = hash === -1 ? url : url.slice(0, hash);
  const frag = hash === -1 ? "" : url.slice(hash);
  const [key] = pair.split("=");
  if (new RegExp(`[?&]${key}=`).test(base)) return url;
  const sep = !base.includes("?") ? "?" : base.endsWith("?") || base.endsWith("&") ? "" : "&";
  return `${base}${sep}${pair}${frag}`;
}

/** `url` asking the daemon for its pane bridge (`?bridge=1`), any query
 *  and fragment it already carries kept. A framed page gets the page API
 *  with it. */
export function withBridge(url: string): string {
  return withPair(url, "bridge=1");
}

/**
 * A preview's own page as Chimaera shows it: with the page API
 * (`window.chimaera`) — and, framed, the pane bridge — appended by the
 * daemon, and the app's theme (`pageTheme.ts`, encoded) for the page to
 * read at once. A framed page hears later themes as messages; a native
 * view's URL carries the current one, so a new theme loads it again.
 */
export function withPageApi(
  url: string,
  opts: { framed: boolean; theme: string | null; inline?: number; key?: string | null },
): string {
  let asked = opts.framed ? withBridge(url) : withPair(url, "api=1");
  // An agent page's key to its folder (`shared/pageNeeds.ts` asked the
  // daemon for it): only a page a host shows inside its guard gets one.
  if (opts.key !== undefined && opts.key !== null && opts.key !== "") asked = withPair(asked, `chimaera-key=${opts.key}`);
  // An inline page (`#inline` on its embed) follows the theme's light/dark
  // for the browser's own defaults too (its text, its controls): it sits on
  // the app's background, not a white card. The value is the most height
  // the card gives it: up to that, the page never shows a scrollbar.
  if (opts.inline !== undefined && opts.inline > 0) asked = withPair(asked, `chimaera-inline=${Math.round(opts.inline)}`);
  return opts.theme === null || opts.theme === "" ? asked : withPair(asked, `chimaera-theme=${opts.theme}`);
}
