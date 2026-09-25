/**
 * Browser notifications for the daemon's notice feed — an agent finished its
 * turn, needs a permission or an answer, stopped on an error, or sent a
 * message through its `notify` tool. Notices arrive as `notices` frames on
 * /ws/events (see `EventsSocket.onNotices`).
 *
 * The native app never takes this path: its shell consumes the same feed
 * straight from each daemon (`GET /api/v1/notices`) and posts real OS
 * notifications — once however many windows are open, and for workspaces
 * with no window at all. A browser has only its tabs, so here
 * every tab receives each notice and the tabs settle among themselves who
 * shows it:
 *
 * - A tab that is focused AND showing the session claims the notice as
 *   seen — nothing is posted anywhere.
 * - Every other tab waits a beat for such a claim, then posts under a
 *   per-notice `tag`, so two tabs posting the same notice collapse into one
 *   alert (the browser replaces same-tag notifications silently).
 * - A newer alert about the same session closes this tab's older one.
 */

import { getSetting } from "../settings/store.svelte";

export type NoticeKind =
  | "done"
  | "input"
  | "permission"
  | "question"
  | "error"
  | "rate_limited"
  | "agent";

/** One notice, as the daemon words it (`chimaera-server/src/notices.rs`). */
export interface Notice {
  id: number;
  kind: NoticeKind;
  /** Blocks the agent on the user (permission, question, waiting). */
  blocking: boolean;
  session_id: string;
  workspace_id: string | null;
  workspace: string | null;
  agent: string | null;
  /** The session's display name (what the rail calls it). */
  name: string;
  title: string;
  subtitle: string;
  body: string;
  at_ms: number;
  age_ms: number;
}

/** What this tab knows when a notice arrives. */
export interface NoticeContext {
  /** Sessions on screen in this tab (each pane's active tab). */
  visible: readonly string[];
  /** Open the session in this tab (the notification was clicked). */
  onClick(sessionId: string): void;
}

/** How long a tab waits for a focused sibling to claim a notice as seen. */
const CLAIM_WAIT_MS = 250;
/** Notices older than this (a tab waking from a long background) are old news. */
const MAX_AGE_MS = 5 * 60 * 1000;

type ChannelMessage = { id: number; seen: boolean; focused: boolean };

let channel: BroadcastChannel | null = null;
/** Per notice id: what focused siblings reported during the claim wait. */
const claims = new Map<number, { seen: boolean; focused: boolean }>();
/** This tab's live alert per session, closed when a newer one supersedes it. */
const shown = new Map<string, Notification>();

function bus(): BroadcastChannel | null {
  if (channel !== null || typeof BroadcastChannel === "undefined") return channel;
  // Scoped by origin already; the host keeps two daemons' tabs apart when a
  // browser reaches both through the same origin (a reused tunnel port).
  channel = new BroadcastChannel(`chimaera-notices:${location.host}`);
  channel.onmessage = (ev: MessageEvent<ChannelMessage>) => {
    const msg = ev.data;
    if (typeof msg?.id !== "number") return;
    const prev = claims.get(msg.id) ?? { seen: false, focused: false };
    claims.set(msg.id, { seen: prev.seen || msg.seen, focused: prev.focused || msg.focused });
  };
  return channel;
}

/** Web Notifications exist here (a secure context — 127.0.0.1 qualifies). */
export function browserNotificationsSupported(): boolean {
  return typeof Notification !== "undefined" && window.isSecureContext;
}

export function browserPermission(): NotificationPermission | "unsupported" {
  return browserNotificationsSupported() ? Notification.permission : "unsupported";
}

/** Ask for permission (must run inside a user gesture in most browsers). */
export async function requestBrowserPermission(): Promise<NotificationPermission | "unsupported"> {
  if (!browserNotificationsSupported()) return "unsupported";
  try {
    return await Notification.requestPermission();
  } catch {
    return Notification.permission;
  }
}

function tabFocused(): boolean {
  return document.visibilityState === "visible" && document.hasFocus();
}

/** Deliver a batch of notices from this tab's events socket. */
export function deliverBrowserNotices(notices: readonly Notice[], ctx: NoticeContext): void {
  if (!browserNotificationsSupported()) return;
  const b = bus();
  for (const n of notices) {
    if (n.age_ms > MAX_AGE_MS) continue;
    const focused = tabFocused();
    const looking = focused && ctx.visible.includes(n.session_id);
    // Tell the siblings what this tab sees before anyone decides.
    b?.postMessage({ id: n.id, seen: looking, focused } satisfies ChannelMessage);
    if (looking) continue;
    setTimeout(() => {
      const claim = claims.get(n.id);
      claims.delete(n.id);
      if (claim?.seen) return;
      const inApp = focused || claim?.focused === true;
      if (inApp && !getSetting("notifications.whileFocused")) return;
      if (Notification.permission !== "granted") return;
      post(n, ctx);
    }, CLAIM_WAIT_MS);
  }
}

function post(n: Notice, ctx: NoticeContext): void {
  const body = [n.subtitle, n.body].filter((s) => s !== "").join("\n");
  let alert: Notification;
  try {
    alert = new Notification(n.title, {
      body,
      tag: `chimaera:${location.host}:${n.id}`,
      silent: !getSetting("notifications.sound"),
    });
  } catch {
    return; // e.g. a browser that only allows notifications from a service worker
  }
  if (n.kind !== "agent") {
    shown.get(n.session_id)?.close();
    shown.set(n.session_id, alert);
  }
  alert.onclick = () => {
    window.focus();
    ctx.onClick(n.session_id);
    alert.close();
  };
  alert.onclose = () => {
    if (shown.get(n.session_id) === alert) shown.delete(n.session_id);
  };
}

/** The user is looking at these sessions: their alerts have done their job. */
export function clearBrowserNotices(sessionIds: readonly string[]): void {
  for (const id of sessionIds) {
    shown.get(id)?.close();
    shown.delete(id);
  }
}
