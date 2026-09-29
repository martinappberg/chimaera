import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { SessionSocket } from "./ws";

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
  // A gateway may close after saying so: the reconnect must not flicker.
  Socket.all[0].close();
  expect(status).toHaveBeenLastCalledWith("asleep");
  vi.advanceTimersByTime(2000);
  const next = Socket.all.at(-1)!;
  next.onopen?.();
  next.onmessage?.({ data: JSON.stringify({ type: "waking" }) });
  expect(status).toHaveBeenLastCalledWith("waking");
  next.onmessage?.({ data: JSON.stringify({ type: "ready", cols: 80, rows: 24 }) });
  expect(status).toHaveBeenLastCalledWith(null);
  next.close();
  expect(status).toHaveBeenLastCalledWith(null);
  session.close();
});
