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

// The account keeps a sleeping cloud machine's sockets open (VIEWING.md, "A
// sleeping cloud machine's sockets") and attaches them again when it wakes.

it("an open socket that says nothing is healthy: no retry and no status change, however long", () => {
  const h = handlers();
  const events = new EventsSocket(h);
  Socket.all[0].onopen?.();
  vi.advanceTimersByTime(60 * 60_000);
  expect(Socket.all).toHaveLength(1);
  expect(h.onStatus).not.toHaveBeenCalled();
  // The machine wakes: its first snapshot brings the socket up...
  Socket.all[0].frame({ type: "sessions", sessions: [] });
  expect(h.onStatus).toHaveBeenLastCalledWith(true);
  // ...and it sleeping again behind the open socket is not a disconnect.
  vi.advanceTimersByTime(60 * 60_000);
  expect(Socket.all).toHaveLength(1);
  expect(h.onStatus).toHaveBeenCalledOnce();
  events.close();
});

it("a gateway view registers again each time the machine attaches to the same socket", () => {
  vi.stubGlobal("location", new URL("https://fixture.invalid/app/worker-one/"));
  const h = { ...handlers(), onSettings: vi.fn() };
  const events = new EventsSocket(h);
  events.watch("w-one");
  events.watchFs(["/project/a.md"], ["/project"]);
  const ws = Socket.all[0];
  ws.onopen?.();
  const watches = (): unknown[] => ws.sent.map((raw) => JSON.parse(raw as string)).filter((frame) => frame.type === "watch");
  const registration = { type: "watch", workspace_id: "w-one", files: ["/project/a.md"], dirs: ["/project"], git_repos: [] };
  expect(watches()).toEqual([registration]);
  // Attached: the snapshots, and the registration again (the first one may
  // have been sent to a machine that was asleep).
  ws.frame({ type: "sessions", sessions: [] });
  ws.frame({ type: "settings", settings: {} });
  expect(watches()).toEqual([registration, registration]);
  // Asleep and awake again behind the same socket: fresh snapshots, and the
  // registration the new attach does not have.
  ws.frame({ type: "sessions", sessions: [{ id: "s-new" }] });
  ws.frame({ type: "settings", settings: {} });
  expect(watches()).toEqual([registration, registration, registration]);
  expect(h.onSessions).toHaveBeenCalledTimes(2);
  expect(h.onSettings).toHaveBeenCalledTimes(2);
  expect(Socket.all).toHaveLength(1);
  events.close();
});

it("a window on its own daemon never repeats its registration", () => {
  const h = handlers();
  const events = new EventsSocket(h);
  events.watch("w-one");
  const ws = Socket.all[0];
  ws.onopen?.();
  ws.frame({ type: "settings", settings: {} });
  ws.frame({ type: "sessions", sessions: [] });
  expect(ws.sent).toHaveLength(2);
  events.close();
});
