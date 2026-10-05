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
    onSendUncertain: vi.fn(),
    onSendConfirmed: vi.fn(),
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

it("uncertain delivery and durable receipts are ordered nonfatal controls, never refusals", async () => {
  const h = handlers();
  const socket = new ChatSocket("s-chat", h);
  Socket.all[0].onopen?.();
  Socket.all[0].frame({ type: "error", code: "send_uncertain", command: "send", client_id: "client-0001", message: "Check delivery." });
  Socket.all[0].frame({ type: "send_confirmed", client_id: "client-0001" });
  await drain();
  expect(h.onSendUncertain).toHaveBeenCalledWith("client-0001", "Check delivery.");
  expect(h.onSendConfirmed).toHaveBeenCalledWith("client-0001");
  expect(h.onCommandFailed).not.toHaveBeenCalled();
  expect(h.onError).not.toHaveBeenCalled();
  expect(socket.healthy).toBe(true);
  socket.close();
});

it("a refused command keeps the socket and reports the refusal", async () => {
  const h = handlers();
  const socket = new ChatSocket("s-chat", h);
  Socket.all[0].onopen?.();
  expect(socket.send({ type: "send", blocks: [] })).toBe(true);
  Socket.all[0].frame({ type: "error", code: "command_failed", message: "not sent", command: "send" });
  Socket.all[0].frame({ type: "error", code: "command_failed", message: "old daemon" });
  // The refused send, named by the id it went out under.
  Socket.all[0].frame({ type: "error", code: "read_only", message: "named", command: "send", client_id: "client-0001" });
  await drain();
  expect(h.onCommandFailed).toHaveBeenCalledWith("not sent", "send", null, null);
  expect(h.onCommandFailed).toHaveBeenCalledWith("old daemon", null, null, null);
  expect(h.onCommandFailed).toHaveBeenCalledWith("named", "send", null, "client-0001");
  expect(socket.healthy).toBe(true);
  socket.close();
});

it("acting brings the work here: the socket says so and a kept refusal names why", async () => {
  const h = handlers();
  h.onBringing = vi.fn();
  const socket = new ChatSocket("s-chat", h);
  Socket.all[0].onopen?.();
  Socket.all[0].frame({ type: "bringing", to: "here" });
  Socket.all[0].frame({
    type: "error",
    code: "command_failed",
    reason: "still_working",
    message: "Your other computer is still working on this. Try again when it pauses.",
    command: "send",
  });
  await drain();
  expect(h.onBringing).toHaveBeenCalledTimes(1);
  expect(h.onCommandFailed).toHaveBeenCalledWith(
    "Your other computer is still working on this. Try again when it pauses.",
    "send",
    "still_working",
    null,
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

it("a send into a browser view whose socket is down reconnects once with wake intent", async () => {
  vi.stubGlobal("location", new URL("https://fixture.invalid/workspace/w-one/"));
  let retire!: (error: Error) => void;
  const held = new Promise<Response>((_resolve, reject) => { retire = reject; });
  vi.stubGlobal("fetch", vi.fn(() => held));
  const socket = new ChatSocket("s-chat", handlers());
  Socket.all[0].onopen?.();
  const pending = readPlacement();
  try {
    // Not authenticated yet: the send is refused (the composer keeps the text).
    expect(socket.send({ type: "send", blocks: [] })).toBe(false);
    expect(socket.send({ type: "send", blocks: [] })).toBe(false);
    expect(Socket.all).toHaveLength(2);
    expect(Socket.all[1].url).toContain("?wake=interaction");
    expect(Socket.all.flatMap((s) => s.sent)).toHaveLength(0);
  } finally {
    socket.close();
    retire(new Error("Fixture placement retired"));
    await expect(pending).rejects.toThrow("Fixture placement retired");
  }
});

it("a frame the store sends by itself never redials or asks for a wake", async () => {
  vi.stubGlobal("location", new URL("https://fixture.invalid/workspace/w-one/"));
  let retire!: (error: Error) => void;
  const held = new Promise<Response>((_resolve, reject) => { retire = reject; });
  vi.stubGlobal("fetch", vi.fn(() => held));
  const socket = new ChatSocket("s-chat", handlers());
  Socket.all[0].onopen?.();
  const pending = readPlacement();
  try {
    // Down (not authenticated yet): the user's own send would dial with wake
    // intent here; a resend or a withdrawal is simply not written.
    expect(socket.sendQuietly({ type: "send", blocks: [], client_id: "client-0001" })).toBe(false);
    expect(socket.sendQuietly({ type: "cancel_send", client_id: "client-0001" })).toBe(false);
    expect(Socket.all).toHaveLength(1);
    expect(Socket.all[0].url).not.toContain("wake=");
    expect(Socket.all.flatMap((s) => s.sent)).toHaveLength(0);
  } finally {
    socket.close();
    retire(new Error("Fixture placement retired"));
    await expect(pending).rejects.toThrow("Fixture placement retired");
  }
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
  // Earlier controlled placement reads settled during their owning teardown.
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

// Against a keeper that keeps a sleeping cloud machine's sockets open (it
// marks them `X-Chimaera-Sockets: kept`; VIEWING.md, "A sleeping cloud
// machine's sockets"), directly or through this computer's relay.

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

it("a ready says whether the daemon takes send ids and whether it is a reattach", async () => {
  const h = handlers();
  const socket = new ChatSocket("s-chat", h);
  Socket.all[0].onopen?.();
  // A daemon that predates send ids says nothing about them.
  Socket.all[0].frame({ type: "ready", session: {}, replay_from: 0, head: 0 });
  // The same socket attached again by whoever keeps it, to a daemon with them.
  Socket.all[0].frame({ type: "ready", session: {}, replay_from: 0, head: 0, send_ids: true });
  await drain();
  expect(h.onReady).toHaveBeenNthCalledWith(1, {}, 0, 0, { sendIds: false, reattach: false });
  expect(h.onReady).toHaveBeenNthCalledWith(2, {}, 0, 0, { sendIds: true, reattach: true });
  // A new socket's first `ready` is a plain reconnect again.
  Socket.all[0].close();
  await vi.advanceTimersByTimeAsync(60_000);
  expect(Socket.all.length).toBeGreaterThan(1);
  Socket.all.at(-1)?.onopen?.();
  Socket.all.at(-1)?.frame({ type: "ready", session: {}, replay_from: 0, head: 0, send_ids: true });
  await drain();
  expect(h.onReady).toHaveBeenLastCalledWith({}, 0, 0, { sendIds: true, reattach: false });
  socket.close();
});

it("the answer to cancel_send reaches the store, in order with the journal", async () => {
  const calls: string[] = [];
  const h = handlers();
  h.onEvent = vi.fn((entry: { seq: number }) => void calls.push(`seq ${entry.seq}`));
  h.onSendCancelled = vi.fn((id: string, cancelled: boolean) => void calls.push(`${id} ${cancelled}`));
  const socket = new ChatSocket("s-chat", h);
  const ws = Socket.all[0];
  ws.onopen?.();
  ws.frame({ type: "ready", session: {}, replay_from: 0, head: 0, send_ids: true });
  ws.frame({ type: "ev", seq: 1, ts: 1, ev: { type: "user_message", text: "one", client_id: "client-0001" } });
  ws.frame({ type: "send_cancelled", client_id: "client-0001", cancelled: false });
  ws.frame({ type: "send_cancelled", client_id: "client-0002", cancelled: true });
  // Not an answer this client can use.
  ws.frame({ type: "send_cancelled", cancelled: true });
  await drain();
  expect(calls).toEqual(["seq 1", "client-0001 false", "client-0002 true"]);
  expect(socket.healthy).toBe(true);
  socket.close();
});

it("a relay that cannot reach the owner is not a kept socket, however long it stays quiet", async () => {
  const h = handlers();
  h.onHeld = vi.fn();
  h.onUnreachable = vi.fn();
  const socket = new ChatSocket("s-chat", h);
  Socket.all[0].onopen?.();
  // The relay's own word, said before the quiet window passed: never kept.
  Socket.all[0].frame({ type: "error", code: "remote_unavailable", reason: "reconnecting", message: "reconnecting" });
  await vi.advanceTimersByTimeAsync(60_000);
  expect(h.onHeld).not.toHaveBeenCalled();
  expect(h.onUnreachable).toHaveBeenCalledOnce();
  socket.close();
  // A slow answer (its probe can take seconds) lands after the socket
  // counted as kept: it ends that, and nothing brings it back but a frame.
  const late = new ChatSocket("s-chat", h);
  Socket.all[1].onopen?.();
  await vi.advanceTimersByTimeAsync(QUIET_OPEN_MS + 50);
  expect(h.onHeld).toHaveBeenCalledOnce();
  Socket.all[1].frame({ type: "error", code: "remote_unavailable", reason: "reconnecting", message: "reconnecting" });
  await vi.advanceTimersByTimeAsync(60_000);
  expect(h.onUnreachable).toHaveBeenCalledTimes(2);
  expect(h.onHeld).toHaveBeenCalledOnce();
  late.close();
});

it("a wake handed back on a kept socket returns to kept, not to reconnecting forever", async () => {
  const calls: string[] = [];
  const h = handlers();
  h.onHeld = vi.fn(() => void calls.push("held"));
  h.onWaking = vi.fn(() => void calls.push("waking"));
  h.onUnreachable = vi.fn(() => void calls.push("unreachable"));
  h.onCommandFailed = vi.fn((_message: string, command: string | null) => void calls.push(`refused ${command}`));
  const socket = new ChatSocket("s-chat", h);
  const ws = Socket.all[0];
  ws.onopen?.();
  await vi.advanceTimersByTimeAsync(QUIET_OPEN_MS + 50);
  expect(socket.send({ type: "send", blocks: [] })).toBe(true);
  expect(socket.send({ type: "send", blocks: [] })).toBe(true);
  ws.frame({ type: "waking" });
  // The wake does not arrive: both held sends come back, then the keeper's
  // "cannot be reached" (no relay's `reason`), and the socket stays open.
  ws.frame({ type: "error", code: "command_failed", message: "Not sent.", command: "send" });
  ws.frame({ type: "error", code: "command_failed", message: "Not sent.", command: "send" });
  ws.frame({ type: "error", code: "remote_unavailable", message: "Your project is reconnecting." });
  await vi.advanceTimersByTimeAsync(QUIET_OPEN_MS + 50);
  expect(calls).toEqual(["held", "waking", "refused send", "refused send", "unreachable", "held"]);
  expect(Socket.all).toHaveLength(1);
  expect(h.onDisconnected).not.toHaveBeenCalled();
  socket.close();
});

it("a slow \"asleep\" after the socket counted as kept is the asleep state, and its close parks", async () => {
  const h = handlers();
  h.onHeld = vi.fn();
  const socket = new ChatSocket("s-chat", h);
  Socket.all[0].onopen?.();
  await vi.advanceTimersByTimeAsync(QUIET_OPEN_MS + 50);
  expect(h.onHeld).toHaveBeenCalledOnce();
  Socket.all[0].frame({ type: "error", code: "worker_asleep", message: "asleep" });
  await vi.advanceTimersByTimeAsync(60_000);
  expect(h.onAsleep).toHaveBeenCalledOnce();
  expect(h.onHeld).toHaveBeenCalledOnce();
  Socket.all[0].close();
  await drain();
  expect(socket.waitingForOwner).toBe(true);
  socket.close();
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
  // This one was kept, so its drop is not "asleep, wait for a send": a
  // keeper that keeps sockets takes the next dial too.
  Socket.all[0].close();
  await drain();
  expect(h.onAsleep).not.toHaveBeenCalled();
  expect(socket.waitingForOwner).toBe(false);
  await vi.advanceTimersByTimeAsync(1_000);
  expect(Socket.all).toHaveLength(2);
  expect(Socket.all[1].url).not.toContain("wake=");
  // A keeper that keeps none refuses it: the old wait, with no timer.
  Socket.all[1].close();
  await drain();
  expect(h.onAsleep).toHaveBeenCalledOnce();
  expect(socket.waitingForOwner).toBe(true);
  await vi.advanceTimersByTimeAsync(10 * 60_000);
  expect(Socket.all).toHaveLength(2);
  socket.close();
});


it("passes bounded active queued IDs at ready and ignores invalid capability data", async () => {
  const h = handlers();
  const socket = new ChatSocket("s-chat", h);
  Socket.all[0].onopen?.();
  Socket.all[0].frame({ type: "ready", session: {}, head: 2, send_ids: true, active_queued_ids: ["client-0001"] });
  await drain();
  expect(h.onReady).toHaveBeenLastCalledWith({}, 0, 2, { sendIds: true, reattach: false, activeQueuedIds: ["client-0001"] });
  for (const invalid of [["bad"], Array(65).fill("client-0001"), "client-0001"]) {
    Socket.all[0].frame({ type: "ready", session: {}, head: 2, send_ids: true, active_queued_ids: invalid });
    await drain();
    expect(h.onReady).toHaveBeenLastCalledWith({}, 0, 2, { sendIds: true, reattach: true });
  }
  socket.close();
});


it("automatic native UI RPCs never wake and an explicit detached control is refused without replay", async () => {
  vi.stubGlobal("location", new URL("https://fixture.invalid/app/worker-one/"));
  const socket = new ChatSocket("s-chat", handlers());
  const ws = Socket.all[0];
  ws.onopen?.();
  ws.frame({ type: "ready", session: {}, replay_from: 0, head: 0 });
  await drain();
  expect(socket.nativeUi.ready).toBe(true);
  ws.readyState = 0;
  try {
    await expect(socket.nativeUi.request({ subtype: "ui_render" })).rejects.toThrow("disconnected");
    expect(Socket.all).toHaveLength(1);
    await expect(socket.nativeUi.request({ subtype: "ui_press", handle: 4 })).rejects.toThrow("disconnected");
    expect(Socket.all).toHaveLength(2);
    expect(Socket.all[1].url).toContain("wake=interaction");
    expect(socket.nativeUi.ready).toBe(false);
    expect(Socket.all.flatMap((s) => s.sent).filter((frame) => String(frame).includes("native_ui"))).toEqual([]);
    Socket.all[1].onopen?.();
    Socket.all[1].frame({ type: "ready", session: {}, replay_from: 0, head: 0 });
    await drain();
    expect(socket.nativeUi.ready).toBe(true);
    expect(Socket.all.flatMap((s) => s.sent).filter((frame) => String(frame).includes("native_ui"))).toEqual([]);
  } finally {
    socket.close();
  }
});

it.each([
  { type: "moved", to: "cloud" },
  { type: "waking" },
  { type: "bringing", to: "here" },
  { type: "paused", reason: "needs_provider", provider: "claude" },
  { type: "error", code: "worker_asleep" },
  { type: "error", code: "remote_unavailable" },
  { type: "error", code: "workspace_scope_changed" },
])("retires native UI handles immediately on %j, including a queued ready", async (state) => {
  const socket = new ChatSocket("s-chat", handlers());
  const ws = Socket.all[0];
  ws.onopen?.();
  ws.frame({ type: "ready", session: {}, replay_from: 0, head: 0 });
  await drain();
  const pending = socket.nativeUi.request({ subtype: "ui_render" });
  const rejected = expect(pending).rejects.toThrow("disconnected");
  // A fresh ready is waiting in the cooperative FIFO when the owner pauses.
  ws.frame({ type: "ready", session: {}, replay_from: 0, head: 0 });
  ws.frame(state);
  expect(socket.nativeUi.ready).toBe(false);
  await rejected;
  await drain();
  expect(socket.nativeUi.ready).toBe(false);
  socket.close();
});
