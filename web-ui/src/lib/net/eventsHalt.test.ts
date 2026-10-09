import { afterEach, beforeEach, expect, it, vi } from "vitest";

import { EventsSocket } from "./events";
import { haltReconnects } from "./reconnect";

// Its own file: the halt is module state for the rest of the page's life.
class Socket {
  static all: Socket[] = [];
  readyState = 1;
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  constructor(readonly url: string) {
    Socket.all.push(this);
  }
  send(): void {}
  close(): void {
    this.onclose?.();
  }
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("WebSocket", Socket);
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

it("the events socket stops dialling once the job window's daemon is gone", () => {
  const events = new EventsSocket({ onSessions: vi.fn(), onStatus: vi.fn(), onFatal: vi.fn() });
  expect(Socket.all).toHaveLength(1);
  // A drop schedules a retry; the halt lands while it is pending.
  Socket.all[0].close();
  haltReconnects();
  vi.advanceTimersByTime(120_000);
  events.retryNow();
  expect(Socket.all).toHaveLength(1);

  // A socket made after the halt never dials.
  const later = new EventsSocket({ onSessions: vi.fn(), onStatus: vi.fn(), onFatal: vi.fn() });
  vi.advanceTimersByTime(120_000);
  expect(Socket.all).toHaveLength(1);
  later.close();
  events.close();
});
