import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SessionSocket } from "./ws";
import { QUIET_OPEN_MS, reconnectingSockets, setSocketKeepers } from "../net/reconnect";
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
// These tests describe a window that can have Pro, where a keeper may hold
// its sockets; one test below switches that off (a public build).
beforeEach(() => { Socket.all = []; vi.useFakeTimers(); vi.stubGlobal("WebSocket", Socket); setSocketKeepers(true); });
afterEach(() => { setSocketKeepers(false); vi.unstubAllGlobals(); vi.useRealTimers(); });
const quiet = { onBinary() {}, onReset() {}, onTitle() {}, onResized() {}, onExited() {}, onError() {} };
it("without keepers a quiet open socket is never taken for a kept one", () => {
  setSocketKeepers(false);
  const kept = vi.fn();
  const session = new SessionSocket("s-fixture", { ...quiet, onKept: kept });
  Socket.all[0].onopen?.();
  vi.advanceTimersByTime(60_000);
  expect(kept).not.toHaveBeenCalled();
  session.close();
});
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

// Against a keeper that keeps a sleeping cloud machine's sockets open (it
// marks them `X-Chimaera-Sockets: kept`; VIEWING.md, "A sleeping cloud
// machine's sockets"), directly or through this computer's relay.

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
  // A wake that does not arrive is handed back ("cannot be reached", from
  // whoever keeps the socket): the waking note ends, and the socket, still
  // open and quiet, is kept again rather than "reconnecting" forever.
  Socket.all[0].onmessage?.({ data: JSON.stringify({ type: "waking" }) });
  expect(status).toHaveBeenLastCalledWith("waking");
  Socket.all[0].onmessage?.({ data: JSON.stringify({ type: "error", code: "remote_unavailable" }) });
  expect(kept).toHaveBeenLastCalledWith(false);
  expect(status).toHaveBeenLastCalledWith(null);
  vi.advanceTimersByTime(QUIET_OPEN_MS);
  expect(kept).toHaveBeenLastCalledWith(true);
  // A relay that cannot reach the owner says it is retrying: that lasts.
  Socket.all[0].onmessage?.({ data: JSON.stringify({ type: "error", code: "remote_unavailable", reason: "reconnecting" }) });
  vi.advanceTimersByTime(60_000);
  expect(kept).toHaveBeenLastCalledWith(false);
  // So does a slow "asleep" from a relay that keeps no socket for the owner.
  Socket.all[0].onmessage?.({ data: JSON.stringify({ type: "ready", cols: 80, rows: 24 }) });
  expect(kept).toHaveBeenLastCalledWith(true);
  Socket.all[0].onmessage?.({ data: JSON.stringify({ type: "error", code: "worker_asleep" }) });
  expect(kept).toHaveBeenLastCalledWith(false);
  expect(status).toHaveBeenLastCalledWith("asleep");
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
  // The machine sleeps and wakes behind the kept socket, attached with a
  // frame that still names the old grid (a resize that had not arrived).
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
  // Parked since, and the pool said so on this socket (`park`): a keeper
  // folds that into the frame it attaches with, so this `ready` answers a
  // parked attach. No snapshot follows, nothing is reset, nothing is sent.
  parked = true;
  session.sendPark();
  let before = ws.sent.length;
  frame({ type: "ready", cols: 80, rows: 24 });
  expect(reset).toHaveBeenCalledOnce();
  expect(parkedReady).toHaveBeenCalledOnce();
  expect(ws.sent).toHaveLength(before);
  // Shown again (`unpark`), then parked while that frame could not go out:
  // the attach said "shown", so the owner would stream to a hidden terminal.
  // It is told to stop, and nothing else: a hidden terminal sends no grid,
  // even when `ready` names another one.
  parked = false;
  session.sendUnpark();
  parked = true;
  before = ws.sent.length;
  frame({ type: "ready", cols: 80, rows: 24 });
  expect(reset).toHaveBeenCalledTimes(2);
  expect(ws.sent.slice(before).map((raw) => JSON.parse(raw as string))).toEqual([{ type: "park" }]);
  expect(parkedReady).toHaveBeenCalledOnce();
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

describe("terminal resize direction", () => {
  const frames = (ws: Socket) => ws.sent.map((raw) => JSON.parse(raw as string));
  function client() {
    let dims = { cols: 100, rows: 30 };
    const resized = (cols?: number, rows?: number) => {
      if (cols === undefined || rows === undefined) return;
      if (cols === dims.cols && rows === dims.rows) return;
      dims = { cols, rows };
      // xterm.resize synchronously emits onResize, including server adoption.
      socket.sendResize(cols, rows);
    };
    const socket = new SessionSocket("s-fixture", { ...quiet, dims: () => dims, onReset: resized, onResized: resized });
    const ws = Socket.all.at(-1)!;
    ws.onopen?.();
    ws.onmessage?.({ data: JSON.stringify({ type: "ready", ...dims }) });
    ws.sent = [];
    return { socket, ws, resized, dims: () => dims };
  }
  const receive = (ws: Socket, frame: object) => ws.onmessage?.({ data: JSON.stringify(frame) });

  it("adopts crossed foreign resizes without reflecting stale sizes to the daemon", () => {
    const a = client();
    // Two other clients have resized before this client receives the events.
    receive(a.ws, { type: "resized", cols: 80, rows: 24 });
    receive(a.ws, { type: "resized", cols: 120, rows: 40 });
    expect(a.dims()).toEqual({ cols: 120, rows: 40 });
    expect(a.ws.sent).toEqual([]);
    a.socket.close();
  });

  it("adopts a snapshot's grid without turning it into a resize request", () => {
    const a = client();
    receive(a.ws, { type: "resync", cols: 80, rows: 24 });
    expect(a.dims()).toEqual({ cols: 80, rows: 24 });
    expect(a.ws.sent).toEqual([]);
    a.socket.close();
  });

  it("reconciles a fit during reconnect without echoing the snapshot's older grid", () => {
    const a = client();
    a.socket.resync();
    const reconnect = Socket.all.at(-1)!;
    expect(reconnect).not.toBe(a.ws);
    reconnect.onopen?.();
    a.resized(110, 35);
    reconnect.sent = [];
    receive(reconnect, { type: "ready", cols: 100, rows: 30 });
    expect(a.dims()).toEqual({ cols: 100, rows: 30 });
    expect(frames(reconnect)).toEqual([{ type: "resize", cols: 110, rows: 35 }]);
    receive(reconnect, { type: "resized", cols: 110, rows: 35 });
    expect(a.dims()).toEqual({ cols: 110, rows: 35 });
    expect(reconnect.sent).toHaveLength(1);
    a.socket.close();
  });

  it("still sends a real fit after receiving a foreign grid", () => {
    const a = client();
    receive(a.ws, { type: "resized", cols: 80, rows: 24 });
    a.ws.sent = [];
    a.resized(110, 35);
    expect(frames(a.ws)).toEqual([{ type: "resize", cols: 110, rows: 35 }]);
    a.socket.close();
  });
});
