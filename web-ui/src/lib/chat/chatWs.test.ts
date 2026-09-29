import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { ChatSocket, type ChatSocketHandlers } from "./chatWs";
import { readPlacement } from "../net/placement";

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

it("a sleeping owner parks the socket: no retry timer, and a send dials with wake intent", async () => {
  const h = handlers();
  const socket = new ChatSocket("s-chat", h);
  Socket.all[0].onopen?.();
  Socket.all[0].frame({ type: "error", code: "worker_asleep", message: "asleep" });
  Socket.all[0].close();
  await drain();
  expect(h.onAsleep).toHaveBeenCalledOnce();
  expect(h.onDisconnected).toHaveBeenCalledOnce();
  expect(socket.waitingForOwner).toBe(true);
  // No backoff churn against a sleeping owner, however long it sleeps.
  await vi.advanceTimersByTimeAsync(10 * 60_000);
  expect(Socket.all).toHaveLength(1);
  expect(socket.healthy).toBe(true);
  // The user's send is what wakes it; the send itself stays with the caller.
  expect(socket.send({ type: "send", blocks: [] })).toBe(false);
  expect(Socket.all).toHaveLength(2);
  expect(Socket.all[1].url).toContain("?wake=interaction");
  expect(socket.waitingForOwner).toBe(false);
  socket.close();
});

it("a parked socket dials passively when its row says the owner answers again", async () => {
  const socket = new ChatSocket("s-chat", handlers());
  Socket.all[0].onopen?.();
  Socket.all[0].frame({ type: "error", code: "worker_asleep", message: "asleep" });
  Socket.all[0].close();
  await drain();
  socket.retrySoon();
  expect(Socket.all).toHaveLength(2);
  expect(Socket.all[1].url).not.toContain("wake=");
  // Still asleep: it parks again, still without a timer.
  Socket.all[1].onopen?.();
  Socket.all[1].frame({ type: "error", code: "worker_asleep", message: "asleep" });
  Socket.all[1].close();
  await vi.advanceTimersByTimeAsync(10 * 60_000);
  expect(Socket.all).toHaveLength(2);
  // A woken owner answers ready: an ordinary drop reconnects on backoff again.
  socket.retrySoon();
  Socket.all[2].onopen?.();
  Socket.all[2].frame({ type: "ready", session: {}, replay_from: 0, head: 0 });
  Socket.all[2].close();
  await vi.advanceTimersByTimeAsync(2_000);
  expect(Socket.all).toHaveLength(4);
  socket.close();
});

it("a project view parks when its placement says the owner sleeps, and dials once it is owned again", async () => {
  // Its own project: an earlier test left a never-answered read for w-one,
  // and reads of one project coalesce.
  vi.stubGlobal("location", new URL("https://fixture.invalid/workspace/w-two/"));
  const suspended = { workspace_id: "w-two", holder_id: "wk", route_host_id: "worker-wk", epoch: 3, policy_revision: 1, availability: "suspended", server_now: "2026-09-28T19:00:00Z", expires_at: "2026-09-28T18:00:00Z" };
  const owned = { ...suspended, availability: "owned", expires_at: "2026-09-28T19:01:30Z" };
  const fetch = vi.fn().mockResolvedValueOnce(Response.json(suspended)).mockResolvedValue(Response.json(owned));
  vi.stubGlobal("fetch", fetch);
  // Reading a response body needs the real setImmediate; only timers are faked.
  vi.useRealTimers();
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval", "Date"] });
  const h = handlers();
  const socket = new ChatSocket("s-chat", h);
  Socket.all[0].onopen?.();
  await readPlacement();
  // The gateway closes without saying why: the placement read already did.
  Socket.all[0].close();
  await drain();
  expect(h.onAsleep).toHaveBeenCalledOnce();
  expect(socket.waitingForOwner).toBe(true);
  await vi.advanceTimersByTimeAsync(60_000);
  expect(Socket.all).toHaveLength(1);
  // Any later read (every action reads it) that finds it owned wakes the wait.
  await readPlacement();
  await vi.advanceTimersByTimeAsync(2_001);
  expect(Socket.all).toHaveLength(2);
  expect(Socket.all[1].url).not.toContain("wake=");
  socket.close();
});
