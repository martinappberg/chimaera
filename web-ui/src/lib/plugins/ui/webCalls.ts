/**
 * Calls from a plugin to its own page, the window's half (API 0.4,
 * `web-call`; docs/features/web-views.md). Each `web` node shown on screen
 * registers here under its plugin, view and node id (and a file view's
 * file); the window's `browser_hello` lists them (`webViews`), so the
 * daemon asks THIS window when it shows the node, with a `browser_request`
 * `web_call` that `browser/agentBrowser.ts` hands to `dispatch`. The node
 * sends its page a `call` command and resolves with the page's
 * `web_answer` — only an answer to an id it sent, at most once.
 *
 * Pure (no DOM, no Svelte), so it is unit-tested.
 */

import type { HelloWebView } from "../../net/browserWire";

/** What a shown node offers: run `method` on its page. */
export interface WebCallTarget {
  plugin: string;
  view: string;
  id: string;
  file: string | null;
  /** Send the page a call; resolves with its value, rejects with its error. */
  call(callId: string, method: string, args: unknown): Promise<unknown>;
}

/** How long a page may take before the window gives up (the daemon gives
 *  up at 45 s; this answers a little before it). */
export const CALL_TIMEOUT_MS = 43_000;
/** Calls one node waits on at once. */
const PENDING_MAX = 16;

const targets = new Set<WebCallTarget>();
const listeners = new Set<() => void>();

function changed(): void {
  for (const fn of listeners) fn();
}

/** Register a shown node; returns the unregister. */
export function registerWebCallTarget(t: WebCallTarget): () => void {
  targets.add(t);
  changed();
  return () => {
    if (targets.delete(t)) changed();
  };
}

/** Hear when the shown nodes change (the hello is re-sent). */
export function onWebViewsChanged(fn: () => void): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

/** The shown nodes, for the hello: deduped, in a stable order. */
export function webViews(): HelloWebView[] {
  const seen = new Map<string, HelloWebView>();
  for (const t of targets) {
    const w = { plugin: t.plugin, view: t.view, id: t.id, file: t.file };
    seen.set(JSON.stringify(w), w);
  }
  return [...seen.entries()].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)).map(([, w]) => w);
}

const text = (v: unknown): v is string => typeof v === "string" && v !== "";

/** A `web_call` request's fields: run it on the node it names. Rejects
 *  (the reply's error) when no node here shows it. */
export async function dispatch(f: Record<string, unknown>): Promise<{ value: unknown }> {
  if (!text(f.plugin) || !text(f.node_view) || !text(f.node) || !text(f.call) || !text(f.method)) {
    throw new Error("malformed web call");
  }
  const file = text(f.file) ? f.file : null;
  const target = [...targets].find(
    (t) => t.plugin === f.plugin && t.view === f.node_view && t.id === f.node && (file === null || t.file === file),
  );
  if (target === undefined) throw new Error("this window no longer shows that view");
  return { value: await target.call(f.call, f.method, f.args ?? null) };
}

/**
 * A node's pending calls: `start` hands back the promise and sends the
 * command; `answer` settles the call it names (an unknown id — a call it
 * never sent, or one already settled — is ignored).
 */
export class PendingCalls {
  private readonly pending = new Map<string, { resolve: (v: unknown) => void; reject: (e: Error) => void; timer: ReturnType<typeof setTimeout> }>();

  start(callId: string, send: () => boolean, timeoutMs = CALL_TIMEOUT_MS): Promise<unknown> {
    if (this.pending.has(callId)) return Promise.reject(new Error("that call is already running"));
    if (this.pending.size >= PENDING_MAX) return Promise.reject(new Error("too many calls are waiting on this page"));
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(callId);
        reject(new Error("the page didn't answer in time"));
      }, timeoutMs);
      this.pending.set(callId, { resolve, reject, timer });
      if (!send()) {
        clearTimeout(timer);
        this.pending.delete(callId);
        reject(new Error("the page isn't running"));
      }
    });
  }

  answer(callId: string, ok: boolean, value: unknown, error: string | null): boolean {
    const p = this.pending.get(callId);
    if (p === undefined) return false;
    this.pending.delete(callId);
    clearTimeout(p.timer);
    if (ok) p.resolve(value);
    else p.reject(new Error(error ?? "the page failed"));
    return true;
  }

  /** The page went away: every waiting call fails now. */
  failAll(why: string): void {
    for (const [id, p] of this.pending) {
      clearTimeout(p.timer);
      p.reject(new Error(why));
      this.pending.delete(id);
    }
  }
}
