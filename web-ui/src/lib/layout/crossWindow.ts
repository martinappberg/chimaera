/**
 * Cross-window tab movement: one adopt protocol, two transports.
 *
 * A move is SENDER-REMOVES-ON-ACK: the receiving window adopts the tabs into
 * its layout and acks; only an ok-ack authorizes the sender to drop its
 * copies. A refused or lost ack leaves the tabs where they were — a surface
 * is never lost, at worst briefly duplicated (and every surface is a view
 * onto daemon-owned state, so a duplicate is just two views).
 *
 * Transports: in the native shell the SHELL routes everything (it alone
 * knows window geometry, so it also powers drag hover/drop across windows);
 * in the browser a BroadcastChannel per (daemon origin, workspace) carries
 * roster + adopt + ack — there is no cross-window drag in a browser, because
 * nothing there can know where sibling windows sit on screen.
 */

import {
  adoptAck,
  adoptTab,
  dragCancel,
  dragDrop,
  dragTrack,
  isNativeShell,
  listScopeWindows,
  onXdrag,
  onXdragAck,
} from "../net/native";

/** A sibling window tabs can move to (same host + workspace). */
export interface XwinWindowInfo {
  winId: string;
  label: string;
  detached: boolean;
}

/** An incoming adoption: the payload is a serialized solo-layout blob
 *  (deserializeLayout validates it; its pane's own `active` index names the
 *  tab to focus). `at` is the drop point in THIS window's client coords —
 *  absent for menu adopts, which land on the focused pane. */
export interface AdoptDrop {
  payload: unknown;
  transfer: number;
  at?: { x: number; y: number };
}

export interface XwinHandlers {
  /** A cross-window drag is hovering this window (native only). */
  onOver(at: { x: number; y: number }): void;
  onLeave(): void;
  onDrop(drop: AdoptDrop): void;
}

export interface XwinTransport {
  /** Route an out-drag's pointer to sibling windows; resolves whether one is
   *  under it (the ghost flips its hint). Browser: always false. */
  track(clientX: number, clientY: number): Promise<boolean>;
  /** Route the drop. Not routed → the caller opens a detached window. */
  drop(
    clientX: number,
    clientY: number,
    payload: unknown,
  ): Promise<{ routed: boolean; transfer?: number }>;
  /** The drag ended without a routed drop — clear any target highlight. */
  cancel(): void;
  roster(): Promise<XwinWindowInfo[]>;
  /** Menu adopt into one window; resolves the transfer id to await. */
  adoptInto(winId: string, payload: unknown): Promise<number>;
  ack(transfer: number, ok: boolean): void;
  /** Incoming protocol traffic addressed to THIS window. */
  incoming(handlers: XwinHandlers): () => void;
  /** Acks for transfers THIS window sent. */
  acks(handler: (transfer: number, ok: boolean) => void): () => void;
  close(): void;
}

// --- sender-side transfer bookkeeping (pure; vitest-covered) ---------------

/** Soft deadline: past this the sender toasts "didn't complete" and keeps
 *  its tabs — but keeps listening (see LATE_MS). */
export const ACK_TIMEOUT_MS = 2000;
/** Hard deadline: a LATE ok-ack inside this window still removes the
 *  sender's copies (the receiver did adopt — leaving both would duplicate);
 *  past it the entry is forgotten entirely. */
export const ACK_LATE_MS = 10_000;

interface LedgerEntry<T> {
  data: T;
  started: number;
  timedOut: boolean;
}

/** Pending sent transfers. Time is a parameter throughout so the policy is
 *  testable without clocks. */
export class TransferLedger<T> {
  private entries = new Map<number, LedgerEntry<T>>();

  add(id: number, data: T, now: number): void {
    this.entries.set(id, { data, started: now, timedOut: false });
  }

  /** An ack arrived. Returns the entry's data when the move should be
   *  APPLIED (ok, known transfer, within the late window); null otherwise.
   *  The entry is consumed either way. */
  ack(id: number, ok: boolean, now: number): T | null {
    const e = this.entries.get(id);
    if (e === undefined) return null;
    this.entries.delete(id);
    if (!ok || now - e.started > ACK_LATE_MS) return null;
    return e.data;
  }

  /** Entries newly past the soft deadline (each reported once — the caller
   *  shows one toast and keeps waiting for a late ack). */
  timeouts(now: number): T[] {
    const out: T[] = [];
    for (const e of this.entries.values()) {
      if (!e.timedOut && now - e.started > ACK_TIMEOUT_MS) {
        e.timedOut = true;
        out.push(e.data);
      }
    }
    return out;
  }

  /** Forget entries past the hard deadline. */
  expire(now: number): void {
    for (const [id, e] of this.entries) {
      if (now - e.started > ACK_LATE_MS) this.entries.delete(id);
    }
  }

  has(id: number): boolean {
    return this.entries.has(id);
  }

  get size(): number {
    return this.entries.size;
  }
}

// --- native transport (shell-routed) ---------------------------------------

export function nativeTransport(): XwinTransport | null {
  if (!isNativeShell()) return null;
  return {
    track: (x, y) => dragTrack(x, y),
    drop: (x, y, payload) => dragDrop(x, y, payload),
    cancel: () => void dragCancel(),
    roster: async () =>
      (await listScopeWindows()).map((w) => ({
        winId: w.win_id,
        label: w.label,
        detached: w.detached,
      })),
    adoptInto: (winId, payload) => adoptTab(winId, payload),
    ack: (transfer, ok) => void adoptAck(transfer, ok),
    incoming: (handlers) => {
      const un = onXdrag((e) => {
        if (e.phase === "over" && e.x !== undefined && e.y !== undefined) {
          handlers.onOver({ x: e.x, y: e.y });
        } else if (e.phase === "leave") {
          handlers.onLeave();
        } else if (e.phase === "drop" && e.transfer !== undefined) {
          handlers.onDrop({
            payload: e.payload,
            transfer: e.transfer,
            at: e.x !== undefined && e.y !== undefined ? { x: e.x, y: e.y } : undefined,
          });
        }
      });
      return () => void un.then((f) => f());
    },
    acks: (handler) => {
      const un = onXdragAck((e) => handler(e.transfer, e.ok));
      return () => void un.then((f) => f());
    },
    close: () => {},
  };
}

// --- browser transport (BroadcastChannel) ----------------------------------

/** How long a roster collect listens for pongs. */
export const ROSTER_COLLECT_MS = 300;

type XwinMessage =
  | { t: "ping" }
  | { t: "pong"; winId: string; label: string; detached: boolean }
  | { t: "adopt"; to: string; from: string; transfer: number; payload: unknown }
  | { t: "ack"; to: string; transfer: number; ok: boolean };

export interface BrowserSelf {
  winId(): string;
  label(): string;
  detached(): boolean;
}

/**
 * The browser transport for one workspace. Same-origin windows on the same
 * daemon share the channel; the workspace id in the name matches the
 * same-scope rule the native shell enforces (a browser window is always on
 * its own daemon origin, so origin covers the host half).
 */
export function broadcastTransport(wsId: string, self: BrowserSelf): XwinTransport {
  const channel = new BroadcastChannel(`chimaera.xwin.${wsId}`);
  let seq = Math.floor(Math.random() * 2 ** 30);
  const ackHandlers = new Set<(transfer: number, ok: boolean) => void>();
  const dropHandlers = new Set<(drop: AdoptDrop) => void>();
  /** Where each received transfer's ack goes (the adopt's sender). */
  const ackTo = new Map<number, string>();

  channel.onmessage = (ev: MessageEvent) => {
    const m = ev.data as XwinMessage;
    if (m.t === "ping") {
      const pong: XwinMessage = {
        t: "pong",
        winId: self.winId(),
        label: self.label(),
        detached: self.detached(),
      };
      channel.postMessage(pong);
    } else if (m.t === "adopt" && m.to === self.winId()) {
      ackTo.set(m.transfer, m.from);
      for (const h of dropHandlers) h({ payload: m.payload, transfer: m.transfer });
    } else if (m.t === "ack" && m.to === self.winId()) {
      for (const h of ackHandlers) h(m.transfer, m.ok);
    }
  };

  return {
    track: () => Promise.resolve(false),
    drop: () => Promise.resolve({ routed: false }),
    cancel: () => {},
    roster: () =>
      new Promise((resolve) => {
        const seen = new Map<string, XwinWindowInfo>();
        const collect = (ev: MessageEvent) => {
          const m = ev.data as XwinMessage;
          if (m.t === "pong" && m.winId !== self.winId()) {
            seen.set(m.winId, { winId: m.winId, label: m.label, detached: m.detached });
          }
        };
        // A second listener so the main onmessage stays undisturbed.
        const extra = new BroadcastChannel(`chimaera.xwin.${wsId}`);
        extra.onmessage = collect;
        channel.postMessage({ t: "ping" } satisfies XwinMessage);
        setTimeout(() => {
          extra.close();
          resolve([...seen.values()]);
        }, ROSTER_COLLECT_MS);
      }),
    adoptInto: (winId, payload) => {
      seq += 1;
      const transfer = seq;
      channel.postMessage({
        t: "adopt",
        to: winId,
        from: self.winId(),
        transfer,
        payload,
      } satisfies XwinMessage);
      return Promise.resolve(transfer);
    },
    ack: (transfer, ok) => {
      const to = ackTo.get(transfer);
      ackTo.delete(transfer);
      if (to === undefined) return; // not a transfer this window received
      channel.postMessage({ t: "ack", to, transfer, ok } satisfies XwinMessage);
    },
    incoming: (handlers) => {
      dropHandlers.add(handlers.onDrop);
      return () => dropHandlers.delete(handlers.onDrop);
    },
    acks: (handler) => {
      ackHandlers.add(handler);
      return () => ackHandlers.delete(handler);
    },
    close: () => channel.close(),
  };
}
