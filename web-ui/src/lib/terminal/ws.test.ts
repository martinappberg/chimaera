import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { SessionSocket } from "./ws";
import { QUIET_OPEN_MS, reconnectingSockets } from "../net/reconnect";
import { get } from "svelte/store";

class Socket {
  static OPEN = 1;
  static all: Socket[] = [];
  readyState = 1;
  binaryType = "";
  sent: unknown[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  constructor(readonly url: string) { Socket.all.push(this); }
  send(value: unknown): void { this.sent.push(value); }
  close(): void { this.onclose?.(); }
}
beforeEach(() => { Socket.all = []; vi.useFakeTimers(); vi.stubGlobal("WebSocket", Socket); });
afterEach(() => { vi.unstubAllGlobals(); vi.useRealTimers(); });
const quiet = { onBinary() {}, onReset() {}, onTitle() {}, onResized() {}, onExited() {}, onError() {} };
it("a viewer adopts server dimensions and never sends input, resize or wake intent", () => {
  let watching = true;
  const reset = vi.fn();
  const session = new SessionSocket("s-fixture", { readOnly: () => watching, dims: () => ({cols: 32, rows: 15}), onBinary() {}, onReset: reset, onTitle() {}, onResized() {}, onExited() {}, onError() {} });
  const first = Socket.all[0];
  expect(first.url).toContain("?read_only=true");
  expect(first.url).not.toContain("wake=");
  first.onopen?.();
  expect(JSON.parse(first.sent[0] as string)).not.toHaveProperty("cols");
  first.onmessage?.({data: JSON.stringify({type: "ready", cols: 120, rows: 40})});
  expect(reset).toHaveBeenCalledWith(120, 40);
  session.sendInput("danger\n"); session.sendResize(30, 10);
  expect(first.sent).toHaveLength(1);
  // Taking control is not interaction: only the first keystroke is.
  watching = false; session.accessChanged();
  const second = Socket.all[1]; expect(second.url).not.toContain("wake=");
  second.onopen?.(); session.sendInput("ok"); expect(second.sent).toHaveLength(2);
  second.onclose?.(); vi.advanceTimersByTime(601);
  expect(Socket.all[2].url).not.toContain("wake=");
  session.close();
});

it("a moved terminal is not exited: it keeps reconnecting to follow the session", () => {
  const exited = vi.fn();
  const session = new SessionSocket("s-fixture", { ...quiet, onExited: exited });
  Socket.all[0].onopen?.();
  Socket.all[0].onmessage?.({ data: JSON.stringify({ type: "moved", to: "cloud" }) });
  Socket.all[0].close();
  vi.advanceTimersByTime(2000);
  expect(exited).not.toHaveBeenCalled();
  expect(Socket.all.length).toBeGreaterThan(1);
  session.close();
});

it("refused typing is reported as a refusal, not a fatal error", () => {
  const refused = vi.fn();
  const error = vi.fn();
  const session = new SessionSocket("s-fixture", { ...quiet, onError: error, onRefused: refused });
  Socket.all[0].onopen?.();
  Socket.all[0].onmessage?.({ data: JSON.stringify({ type: "error", code: "read_only", reason: "busy", message: "busy" }) });
  expect(refused).toHaveBeenCalledWith("busy", "busy");
  expect(error).not.toHaveBeenCalled();
  session.close();
});

it("opening a terminal is passive everywhere", () => {
  const session = new SessionSocket("s-fixture", quiet);
  expect(Socket.all[0].url).not.toContain("wake=");
  session.close();
});

it("typing into a browser view with its socket down reconnects once with wake intent", () => {
  vi.stubGlobal("location", new URL("https://fixture.invalid/workspace/w-one/"));
  // Placement never answers: the socket opens but never authenticates.
  vi.stubGlobal("fetch", vi.fn(() => new Promise<Response>(() => {})));
  const session = new SessionSocket("s-fixture", quiet);
  Socket.all[0].onopen?.();
  session.sendInput("a");
  session.sendInput("b");
  expect(Socket.all).toHaveLength(2);
  expect(Socket.all[1].url).toContain("?wake=interaction");
  expect(Socket.all[0].sent).toHaveLength(0);
  expect(Socket.all[1].sent).toHaveLength(0);
  session.close();
});

it("a browser view says the dropped keystroke is waking the project", () => {
  vi.stubGlobal("location", new URL("https://fixture.invalid/workspace/w-one/"));
  vi.stubGlobal("fetch", vi.fn(() => new Promise<Response>(() => {})));
  const refused = vi.fn();
  const session = new SessionSocket("s-fixture", { ...quiet, onRefused: refused });
  Socket.all[0].onopen?.();
  session.sendInput("a");
  expect(refused).toHaveBeenCalledWith("waking", null);
  session.close();
});

it("waking is a lasting state until the session answers, and input is not live before", () => {
  const status = vi.fn();
  const session = new SessionSocket("s-fixture", { ...quiet, onStatus: status });
  Socket.all[0].onopen?.();
  expect(session.isOpen).toBe(true);
  expect(session.isLive).toBe(false);
  Socket.all[0].onmessage?.({ data: JSON.stringify({ type: "waking" }) });
  expect(status).toHaveBeenLastCalledWith("waking");
  Socket.all[0].onmessage?.({ data: JSON.stringify({ type: "ready", cols: 80, rows: 24 }) });
  expect(status).toHaveBeenLastCalledWith(null);
  expect(session.isLive).toBe(true);
  session.close();
});

it("a native window never reconnects early on typing: its daemon holds the input", () => {
  const session = new SessionSocket("s-fixture", quiet);
  Socket.all[0].readyState = 0;
  session.sendInput("a");
  expect(Socket.all).toHaveLength(1);
  session.close();
});

it("an asleep owner is a lasting state that outlives a dropped socket", () => {
  const status = vi.fn();
  const error = vi.fn();
  const session = new SessionSocket("s-fixture", { ...quiet, onError: error, onStatus: status });
  Socket.all[0].onopen?.();
  Socket.all[0].onmessage?.({ data: JSON.stringify({ type: "error", code: "worker_asleep", message: "asleep" }) });
  expect(status).toHaveBeenLastCalledWith("asleep");
  expect(error).not.toHaveBeenCalled();
  // A gateway may close after saying so: the status must not flicker, and
  // no retry timer runs against a sleeping owner.
  Socket.all[0].close();
  expect(status).toHaveBeenLastCalledWith("asleep");
  expect(session.waitingForOwner).toBe(true);
  vi.advanceTimersByTime(10 * 60_000);
  expect(Socket.all).toHaveLength(1);
  // Its row says the owner answers again: dial once, passively.
  session.retrySoon();
  const next = Socket.all.at(-1)!;
  expect(Socket.all).toHaveLength(2);
  expect(next.url).not.toContain("wake=");
  next.onopen?.();
  next.onmessage?.({ data: JSON.stringify({ type: "waking" }) });
  expect(status).toHaveBeenLastCalledWith("waking");
  next.onmessage?.({ data: JSON.stringify({ type: "ready", cols: 80, rows: 24 }) });
  expect(status).toHaveBeenLastCalledWith(null);
  next.close();
  expect(status).toHaveBeenLastCalledWith(null);
  session.close();
});

it("a keystroke into a terminal waiting on a sleeping owner dials once with wake intent", () => {
  const refused = vi.fn();
  const session = new SessionSocket("s-fixture", { ...quiet, onRefused: refused });
  Socket.all[0].onopen?.();
  Socket.all[0].onmessage?.({ data: JSON.stringify({ type: "error", code: "worker_asleep" }) });
  Socket.all[0].close();
  session.sendInput("a");
  session.sendInput("b");
  expect(Socket.all).toHaveLength(2);
  expect(Socket.all[1].url).toContain("?wake=interaction");
  expect(refused).toHaveBeenCalledWith("waking", null);
  expect(session.waitingForOwner).toBe(false);
  session.close();
});

// The account keeps a sleeping cloud machine's sockets open (VIEWING.md, "A
// sleeping cloud machine's sockets"); so does this computer's relay to it.

it("typing into a socket kept open for an owner that has not answered is sent, never dropped", () => {
  const kept = vi.fn();
  const refused = vi.fn();
  const status = vi.fn();
  const session = new SessionSocket("s-fixture", { ...quiet, onKept: kept, onRefused: refused, onStatus: status });
  Socket.all[0].onopen?.();
  vi.advanceTimersByTime(QUIET_OPEN_MS - 1);
  expect(kept).not.toHaveBeenCalled();
  vi.advanceTimersByTime(1);
  expect(kept).toHaveBeenLastCalledWith(true);
  // However long it stays quiet: no retry, no parked wait, no indicator.
  vi.advanceTimersByTime(60 * 60_000);
  expect(Socket.all).toHaveLength(1);
  expect(session.waitingForOwner).toBe(false);
  expect(get(reconnectingSockets)).toBe(0);
  session.sendInput("ls\r");
  session.sendInput("pwd\r");
  expect(Socket.all).toHaveLength(1);
  expect(Socket.all[0].sent).toHaveLength(3);
  expect(refused).not.toHaveBeenCalled();
  expect(status).not.toHaveBeenCalled();
  // Open, but nothing echoes for input the owner has not received.
  expect(session.isLive).toBe(false);
  // Being told the owner cannot be reached ends the kept state and a wake
  // that was under way.
  Socket.all[0].onmessage?.({ data: JSON.stringify({ type: "waking" }) });
  expect(status).toHaveBeenLastCalledWith("waking");
  Socket.all[0].onmessage?.({ data: JSON.stringify({ type: "error", code: "remote_unavailable" }) });
  expect(kept).toHaveBeenLastCalledWith(false);
  expect(status).toHaveBeenLastCalledWith(null);
  session.close();
});

it("a second ready on the same socket repaints like a reconnect and settles a grid or park that changed since", () => {
  let parked = false;
  let dims = { cols: 80, rows: 24 };
  const reset = vi.fn();
  const parkedReady = vi.fn();
  const status = vi.fn();
  const session = new SessionSocket("s-fixture", { ...quiet, parked: () => parked, dims: () => dims, onReset: reset, onParkedReady: parkedReady, onStatus: status });
  const ws = Socket.all[0];
  const frame = (value: unknown): void => ws.onmessage?.({ data: JSON.stringify(value) });
  const last = (): unknown => JSON.parse(ws.sent.at(-1) as string);
  ws.onopen?.();
  frame({ type: "ready", cols: 80, rows: 24 });
  expect(reset).not.toHaveBeenCalled();
  // The machine sleeps and wakes behind the kept socket. The account replays
  // the auth frame as first sent, so the owner renders at the old grid.
  dims = { cols: 100, rows: 30 };
  session.sendInput("x");
  frame({ type: "waking" });
  expect(status).toHaveBeenLastCalledWith("waking");
  expect(session.isLive).toBe(false);
  frame({ type: "ready", cols: 80, rows: 24 });
  expect(status).toHaveBeenLastCalledWith(null);
  expect(session.isLive).toBe(true);
  // Reset before the snapshot that follows, at the grid it was rendered at...
  expect(reset).toHaveBeenCalledExactlyOnceWith(80, 24);
  // ...and the terminal's real grid goes back to the owner.
  expect(last()).toEqual({ type: "resize", cols: 100, rows: 30 });
  // Parked since: the replayed frame said "shown", so the owner would stream
  // to a hidden terminal. It is told to stop.
  parked = true;
  frame({ type: "ready", cols: 100, rows: 30 });
  expect(reset).toHaveBeenCalledTimes(2);
  expect(last()).toEqual({ type: "park" });
  expect(parkedReady).not.toHaveBeenCalled();
  expect(Socket.all).toHaveLength(1);
  session.close();
});

it("a terminal shown since its parked attach asks for the repaint it was not sent", () => {
  let parked = true;
  const reset = vi.fn();
  const parkedReady = vi.fn();
  const session = new SessionSocket("s-fixture", { ...quiet, parked: () => parked, dims: () => ({ cols: 90, rows: 20 }), onReset: reset, onParkedReady: parkedReady });
  const ws = Socket.all[0];
  ws.onopen?.();
  expect(JSON.parse(ws.sent[0] as string)).toMatchObject({ parked: true });
  parked = false;
  ws.onmessage?.({ data: JSON.stringify({ type: "ready", cols: 80, rows: 24 }) });
  expect(ws.sent.slice(1).map((raw) => JSON.parse(raw as string))).toEqual([{ type: "resize", cols: 90, rows: 20 }, { type: "unpark" }]);
  expect(parkedReady).not.toHaveBeenCalled();
  // The repaint arrives as a resync, which resets; `ready` itself did not.
  expect(reset).not.toHaveBeenCalled();
  session.close();
});

it("a still-parked attach is unchanged: no snapshot, no reset, the buffer desyncs", () => {
  const reset = vi.fn();
  const parkedReady = vi.fn();
  const session = new SessionSocket("s-fixture", { ...quiet, parked: () => true, dims: () => ({ cols: 90, rows: 20 }), onReset: reset, onParkedReady: parkedReady });
  Socket.all[0].onopen?.();
  Socket.all[0].onmessage?.({ data: JSON.stringify({ type: "ready", cols: 80, rows: 24 }) });
  expect(parkedReady).toHaveBeenCalledOnce();
  expect(reset).not.toHaveBeenCalled();
  expect(Socket.all[0].sent).toHaveLength(1);
  session.close();
});
