import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { EventsSocket, type EventsSocketHandlers } from "./events";

class Socket {
  static OPEN = 1;
  static all: Socket[] = [];
  readyState = 1;
  sent: unknown[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  constructor(readonly url: string) {
    Socket.all.push(this);
  }
  send(value: unknown): void {
    this.sent.push(value);
  }
  close(): void {
    this.onclose?.();
  }
  frame(value: unknown): void {
    this.onmessage?.({ data: JSON.stringify(value) });
  }
}

beforeEach(() => {
  Socket.all = [];
  vi.useFakeTimers();
  vi.stubGlobal("WebSocket", Socket);
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

function handlers() {
  const onFatal = vi.fn<(message: string) => void>();
  const h: EventsSocketHandlers = { onSessions: vi.fn(), onStatus: vi.fn(), onFatal };
  return { ...h, onFatal };
}

it("a project connection changing is not fatal: the events socket reconnects", () => {
  for (const code of ["remote_unavailable", "workspace_scope_changed"]) {
    Socket.all = [];
    const h = handlers();
    const events = new EventsSocket(h);
    Socket.all[0].onopen?.();
    Socket.all[0].frame({ type: "error", code, message: "changed" });
    Socket.all[0].close();
    vi.advanceTimersByTime(15_000);
    expect(h.onFatal).not.toHaveBeenCalled();
    expect(Socket.all.length).toBeGreaterThan(1);
    events.close();
  }
});

it("a rejected socket still gives up and says why", () => {
  const h = handlers();
  const events = new EventsSocket(h);
  Socket.all[0].onopen?.();
  Socket.all[0].frame({ type: "error", message: "unauthorized" });
  vi.advanceTimersByTime(15_000);
  expect(h.onFatal).toHaveBeenCalledWith("unauthorized");
  expect(Socket.all).toHaveLength(1);
  events.close();
});
