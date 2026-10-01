import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { ChatSocket, type ChatSocketHandlers } from "./chatWs";
import { readPlacement } from "../net/placement";
import { QUIET_OPEN_MS, reconnectingSockets } from "../net/reconnect";
import { get } from "svelte/store";

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
  expect(h.onCommandFailed).toHaveBeenCalledWith("not sent", "send", null);
  expect(h.onCommandFailed).toHaveBeenCalledWith("old daemon", null, null);
  expect(socket.healthy).toBe(true);
  socket.close();
});

it("acting brings the work here: the socket says so and a kept refusal names why", async () => {
  const h = handlers();
  h.onBringing = vi.fn();
  const socket = new ChatSocket("s-chat", h);
  Socket.all[0].onopen?.();
  Socket.all[0].frame({ type: "bringing", to: "here" });
  Socket.all[0].frame({ type: "bringing", to: "computer" });
  Socket.all[0].frame({
    type: "error",
    code: "command_failed",
    reason: "still_working",
    message: "Your other computer is still working on this. Try again when it pauses.",
    command: "send",
  });
  await drain();
  expect(h.onBringing).toHaveBeenNthCalledWith(1, "here");
  expect(h.onBringing).toHaveBeenNthCalledWith(2, "computer");
  expect(h.onCommandFailed).toHaveBeenCalledWith(
    "Your other computer is still working on this. Try again when it pauses.",
    "send",
    "still_working",
  );
  expect(socket.healthy).toBe(true);
  socket.close();
});

it("work that went to another of the user's computers says so", async () => {
  const h = handlers();
  const socket = new ChatSocket("s-chat", h);
  Socket.all[0].onopen?.();
  Socket.all[0].frame({ type: "moved", to: "computer", other: true });
  await drain();
  expect(h.onMoved).toHaveBeenCalledWith("other");
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

// The account keeps a sleeping cloud machine's sockets open (VIEWING.md, "A
// sleeping cloud machine's sockets"); so does this computer's relay to it.

it("a socket kept open for an owner that has not answered is healthy, and a send goes out on it", async () => {
  const h = handlers();
  h.onHeld = vi.fn();
  const socket = new ChatSocket("s-chat", h);
  Socket.all[0].onopen?.();
  await vi.advanceTimersByTimeAsync(QUIET_OPEN_MS - 1);
  expect(h.onHeld).not.toHaveBeenCalled();
  await vi.advanceTimersByTimeAsync(51);
  expect(h.onHeld).toHaveBeenCalledOnce();
  // However long it stays quiet: no retry, no parked wait, no indicator.
  await vi.advanceTimersByTimeAsync(60 * 60_000);
  expect(Socket.all).toHaveLength(1);
  expect(socket.waitingForOwner).toBe(false);
  expect(get(reconnectingSockets)).toBe(0);
  expect(h.onDisconnected).not.toHaveBeenCalled();
  expect(h.onAsleep).not.toHaveBeenCalled();
  // The send is simply sent (whoever keeps the socket holds it): no redial.
  expect(socket.send({ type: "send", blocks: [] })).toBe(true);
  expect(socket.send({ type: "permission", request_id: "r", option_id: "allow_once" })).toBe(true);
  expect(Socket.all).toHaveLength(1);
  expect(Socket.all[0].sent).toHaveLength(3);
  socket.close();
});

it("a second ready on the same socket is one more ready, in order with the gap it brings", async () => {
  const calls: string[] = [];
  const h = handlers();
  h.onReady = vi.fn((_session: unknown, replayFrom: number) => void calls.push(`ready from ${replayFrom}`));
  h.onEvent = vi.fn((entry: { seq: number }) => void calls.push(`seq ${entry.seq}`));
  h.onWaking = vi.fn(() => void calls.push("waking"));
  const socket = new ChatSocket("s-chat", h);
  const ws = Socket.all[0];
  ws.onopen?.();
  ws.frame({ type: "ready", session: {}, replay_from: 0, head: 1 });
  ws.frame({ type: "batch", events: [{ seq: 1, ts: 1, ev: { type: "user_message", text: "one" } }] });
  // The machine sleeps and wakes behind the kept socket.
  expect(socket.send({ type: "send", blocks: [] })).toBe(true);
  ws.frame({ type: "waking" });
  ws.frame({ type: "ready", session: {}, replay_from: 1, head: 1 });
  ws.frame({ type: "ev", seq: 2, ts: 2, ev: { type: "user_message", text: "two" } });
  await drain();
  expect(calls).toEqual(["ready from 0", "seq 1", "waking", "ready from 1", "seq 2"]);
  expect(h.onDisconnected).not.toHaveBeenCalled();
  expect(Socket.all).toHaveLength(1);
  socket.close();
});

it("an owner that cannot be reached is not a kept socket", async () => {
  const h = handlers();
  h.onHeld = vi.fn();
  h.onUnreachable = vi.fn();
  const socket = new ChatSocket("s-chat", h);
  Socket.all[0].onopen?.();
  // Said before the quiet window passed: never counted as kept.
  Socket.all[0].frame({ type: "error", code: "remote_unavailable", message: "reconnecting" });
  await vi.advanceTimersByTimeAsync(QUIET_OPEN_MS + 50);
  expect(h.onHeld).not.toHaveBeenCalled();
  expect(h.onUnreachable).toHaveBeenCalledOnce();
  socket.close();
  // Said after it (a relay whose probe timed out): the kept state ends.
  const late = new ChatSocket("s-chat", h);
  Socket.all[1].onopen?.();
  await vi.advanceTimersByTimeAsync(QUIET_OPEN_MS + 50);
  expect(h.onHeld).toHaveBeenCalledOnce();
  Socket.all[1].frame({ type: "error", code: "remote_unavailable", message: "reconnecting" });
  await drain();
  expect(h.onUnreachable).toHaveBeenCalledTimes(2);
  late.close();
});

it("a kept socket that drops while the owner sleeps is dialed again, and only a refused dial parks", async () => {
  vi.stubGlobal("location", new URL("https://fixture.invalid/workspace/w-three/"));
  const suspended = { workspace_id: "w-three", holder_id: "wk", route_host_id: "worker-wk", epoch: 3, policy_revision: 1, availability: "suspended", server_now: "2026-09-28T19:00:00Z", expires_at: "2026-09-28T18:00:00Z" };
  vi.stubGlobal("fetch", vi.fn(() => Promise.resolve(Response.json(suspended))));
  vi.useRealTimers();
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval", "Date"] });
  const h = handlers();
  h.onHeld = vi.fn();
  const socket = new ChatSocket("s-chat", h);
  Socket.all[0].onopen?.();
  await readPlacement();
  await vi.advanceTimersByTimeAsync(QUIET_OPEN_MS + 50);
  expect(h.onHeld).toHaveBeenCalledOnce();
  // The account kept this one, so its drop is not "asleep, wait for a send":
  // an account that keeps sockets takes the next dial too.
  Socket.all[0].close();
  await drain();
  expect(h.onAsleep).not.toHaveBeenCalled();
  expect(socket.waitingForOwner).toBe(false);
  await vi.advanceTimersByTimeAsync(1_000);
  expect(Socket.all).toHaveLength(2);
  expect(Socket.all[1].url).not.toContain("wake=");
  // An account that keeps none refuses it: the old wait, with no timer.
  Socket.all[1].close();
  await drain();
  expect(h.onAsleep).toHaveBeenCalledOnce();
  expect(socket.waitingForOwner).toBe(true);
  await vi.advanceTimersByTimeAsync(10 * 60_000);
  expect(Socket.all).toHaveLength(2);
  socket.close();
});
