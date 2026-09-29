import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { ChatSocket, type ChatSocketHandlers } from "./chatWs";

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

function handlers(): ChatSocketHandlers & Record<string, ReturnType<typeof vi.fn>> {
  return {
    onReady: vi.fn(),
    onEvent: vi.fn(),
    onDegraded: vi.fn(),
    onExited: vi.fn(),
    onError: vi.fn(),
    onCommandFailed: vi.fn(),
    onAsleep: vi.fn(),
    onMoved: vi.fn(),
    onPaused: vi.fn(),
    onDisconnected: vi.fn(),
    lastSeq: () => 0,
  } as unknown as ChatSocketHandlers & Record<string, ReturnType<typeof vi.fn>>;
}

/** Deliveries are applied cooperatively; let the queue drain. */
async function drain(): Promise<void> {
  await vi.advanceTimersByTimeAsync(50);
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

it("attaching is passive and a paused owner is a state, not an error", async () => {
  const h = handlers();
  const socket = new ChatSocket("s-chat", h);
  expect(Socket.all[0].url).not.toContain("wake=");
  Socket.all[0].onopen?.();
  Socket.all[0].frame({ type: "error", code: "worker_asleep", message: "paused" });
  Socket.all[0].frame({ type: "error", code: "remote_unavailable", message: "reconnecting" });
  await drain();
  expect(h.onAsleep).toHaveBeenCalledOnce();
  expect(h.onError).not.toHaveBeenCalled();
  expect(socket.healthy).toBe(true);
  socket.close();
});

it("a moved conversation stays healthy and reconnects to follow it", async () => {
  const h = handlers();
  const socket = new ChatSocket("s-chat", h);
  Socket.all[0].onopen?.();
  Socket.all[0].frame({ type: "moved", to: "computer" });
  // Nothing may be sent into a socket that is about to close.
  expect(socket.send({ type: "send", blocks: [] })).toBe(false);
  Socket.all[0].close();
  await drain();
  expect(h.onMoved).toHaveBeenCalledWith("computer");
  expect(h.onExited).not.toHaveBeenCalled();
  expect(socket.healthy).toBe(true);
  await vi.advanceTimersByTimeAsync(2000);
  expect(Socket.all.length).toBeGreaterThan(1);
  socket.close();
});

it("a paused conversation is not an exit and reconnects at once when it is reachable", async () => {
  const h = handlers();
  const socket = new ChatSocket("s-chat", h);
  Socket.all[0].onopen?.();
  Socket.all[0].frame({ type: "paused", reason: "needs_provider", provider: "claude" });
  expect(socket.send({ type: "send", blocks: [] })).toBe(false);
  Socket.all[0].close();
  await drain();
  expect(h.onPaused).toHaveBeenCalledWith({ type: "paused", reason: "needs_provider", provider: "claude" });
  expect(h.onExited).not.toHaveBeenCalled();
  expect(socket.healthy).toBe(true);
  // Several paused answers grow the backoff...
  for (let i = 1; i <= 4; i++) {
    await vi.advanceTimersByTimeAsync(20_000);
    Socket.all.at(-1)?.close();
  }
  const before = Socket.all.length;
  // ...but the row coming back retries now, not after the rest of it.
  socket.retrySoon();
  await vi.advanceTimersByTimeAsync(1);
  expect(Socket.all.length).toBe(before + 1);
  socket.close();
});

it("a refused command keeps the socket and reports the refusal", async () => {
  const h = handlers();
  const socket = new ChatSocket("s-chat", h);
  Socket.all[0].onopen?.();
  expect(socket.send({ type: "send", blocks: [] })).toBe(true);
  Socket.all[0].frame({ type: "error", code: "command_failed", message: "not sent", command: "send" });
  Socket.all[0].frame({ type: "error", code: "command_failed", message: "old daemon" });
  await drain();
  expect(h.onCommandFailed).toHaveBeenCalledWith("not sent", "send");
  expect(h.onCommandFailed).toHaveBeenCalledWith("old daemon", null);
  expect(socket.healthy).toBe(true);
  socket.close();
});

it("a send into a browser view whose socket is down reconnects once with wake intent", () => {
  vi.stubGlobal("location", new URL("https://fixture.invalid/workspace/w-one/"));
  vi.stubGlobal("fetch", vi.fn(() => new Promise<Response>(() => {})));
  const socket = new ChatSocket("s-chat", handlers());
  Socket.all[0].onopen?.();
  // Not authenticated yet: the send is refused (the composer keeps the text).
  expect(socket.send({ type: "send", blocks: [] })).toBe(false);
  expect(socket.send({ type: "send", blocks: [] })).toBe(false);
  expect(Socket.all).toHaveLength(2);
  expect(Socket.all[1].url).toContain("?wake=interaction");
  expect(Socket.all.flatMap((s) => s.sent)).toHaveLength(0);
  socket.close();
});

it("a native window's send never reconnects early", () => {
  const socket = new ChatSocket("s-chat", handlers());
  Socket.all[0].readyState = 0;
  expect(socket.send({ type: "send", blocks: [] })).toBe(false);
  expect(Socket.all).toHaveLength(1);
  socket.close();
});
