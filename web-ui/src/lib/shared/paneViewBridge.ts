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
  | { id: string; kind: "user_input" };

const isObj = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null;
const str = (v: unknown): v is string => typeof v === "string";
const bool = (v: unknown): v is boolean => typeof v === "boolean";
const num = (v: unknown): v is number => typeof v === "number" && Number.isFinite(v);

function rectOf(v: unknown): ViewRectLike | null {
  if (!isObj(v) || !num(v.x) || !num(v.y) || !num(v.width) || !num(v.height)) return null;
  return { x: v.x, y: v.y, width: v.width, height: v.height };
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

/** `url` asking the daemon for its pane bridge (`?bridge=1`), any query
 *  and fragment it already carries kept. */
export function withBridge(url: string): string {
  const hash = url.indexOf("#");
  const base = hash === -1 ? url : url.slice(0, hash);
  const frag = hash === -1 ? "" : url.slice(hash);
  if (/[?&]bridge=1(&|$)/.test(base)) return url;
  const sep = !base.includes("?") ? "?" : base.endsWith("?") || base.endsWith("&") ? "" : "&";
  return `${base}${sep}bridge=1${frag}`;
}
