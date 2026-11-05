/**
 * The agent browser's wire on `/ws/events` (#273), client side: the
 * daemon's `browser_request` frame and `browser_links` (on the sessions
 * frame), and the client messages a window sends back (`browser_reply`,
 * `browser_event`, `browser_hello`). The daemon builds the frame in
 * `crates/chimaera-server/src/ws/frames.rs` and reads the messages as
 * `ws.rs` `ClientMessage`; `fixtures/wire/` pins the frame. What a window
 * does with a request is `browser/agentBrowser.ts`.
 */

const isObj = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null;
const optId = (v: unknown): v is string | null | undefined =>
  v === undefined || v === null || (typeof v === "string" && v !== "");

/** One `{"type":"browser_request", ...}` frame. The envelope is checked
 *  here; each op's own fields are checked by the handler, which answers a
 *  malformed one with an error instead of leaving the daemon to time out. */
export interface BrowserRequest {
  rid: number;
  op: string;
  /** The pane-view id (an agent's own or handed-over page). */
  view: string | null;
  /** A shared pane's BrowserTab id. */
  tab: string | null;
  /** The whole frame: the op's own fields. */
  fields: Record<string, unknown>;
}

/** Validate a frame; null for anything without a usable envelope (dropped:
 *  with no `rid` there is nothing to answer). */
export function decodeBrowserRequest(raw: unknown): BrowserRequest | null {
  if (!isObj(raw) || raw.type !== "browser_request") return null;
  const { rid, op, view, tab } = raw;
  if (typeof rid !== "number" || !Number.isSafeInteger(rid) || rid < 0) return null;
  if (typeof op !== "string" || op === "") return null;
  if (!optId(view) || !optId(tab)) return null;
  return { rid, op, view: view ?? null, tab: tab ?? null, fields: raw };
}

/** A browser tab the user shared with an agent (the sessions frame's
 *  additive `browser_links`). */
export interface BrowserLink {
  tab: string;
  agent_id: string;
}

/** The sessions frame's `browser_links`: undefined when absent (a daemon
 *  predating the agent browser), malformed entries dropped. */
export function decodeBrowserLinks(raw: unknown): BrowserLink[] | undefined {
  if (!Array.isArray(raw)) return undefined;
  const out: BrowserLink[] = [];
  for (const l of raw) {
    if (isObj(l) && typeof l.tab === "string" && l.tab !== "" && typeof l.agent_id === "string" && l.agent_id !== "") {
      out.push({ tab: l.tab, agent_id: l.agent_id });
    }
  }
  return out;
}

export type BrowserReply =
  | { type: "browser_reply"; rid: number; ok: true; result: unknown }
  | { type: "browser_reply"; rid: number; ok: false; error: string };

export type BrowserEventKind = "takeover" | "resume" | "view_lost";

export interface BrowserEventMsg {
  type: "browser_event";
  kind: BrowserEventKind;
  view: string | null;
  tab: string | null;
}

/** What this window can do for the agent browser, re-sent whenever it
 *  changes and after every (re)auth: the daemon picks a window by it. */
export interface BrowserHello {
  type: "browser_hello";
  /** Pane views are possible here (the macOS app). */
  app_engine: boolean;
  visible: boolean;
  focused: boolean;
  width: number;
  height: number;
  /** Browser tabs with a live pane view in this window (sorted). */
  tabs: string[];
  /** What each of `tabs` shows (same order; agents may read any pane in
   *  their workspace): its title (≤ 200 chars, host:port when it has none)
   *  and its address in the app's terms, `http://host:port/path` — never
   *  the `/proxy/` URL, and no query (a token rides there). */
  pages?: BrowserPage[];
}

export interface BrowserPage {
  tab: string;
  title: string;
  url: string;
}

export type BrowserClientMessage = BrowserReply | BrowserEventMsg | BrowserHello;

/** Whether a hello says nothing new (no send). */
export function sameHello(a: BrowserHello | null, b: BrowserHello): boolean {
  return (
    a !== null &&
    a.app_engine === b.app_engine &&
    a.visible === b.visible &&
    a.focused === b.focused &&
    a.width === b.width &&
    a.height === b.height &&
    a.tabs.length === b.tabs.length &&
    a.tabs.every((t, i) => t === b.tabs[i]) &&
    samePages(a.pages ?? [], b.pages ?? [])
  );
}

function samePages(a: BrowserPage[], b: BrowserPage[]): boolean {
  return (
    a.length === b.length &&
    a.every((p, i) => p.tab === b[i].tab && p.title === b[i].title && p.url === b[i].url)
  );
}
