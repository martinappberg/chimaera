import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SessionSocket } from "./ws";

vi.mock("../net/api", () => ({ getToken: () => "test-token" }));

class FakeWebSocket {
  static OPEN = 1;
  static instances: FakeWebSocket[] = [];
  readyState = 0;
  binaryType = "";
  onopen: (() => void) | null = null;
  onmessage: ((ev: { data: string | ArrayBuffer }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  sent: unknown[] = [];
  constructor() { FakeWebSocket.instances.push(this); }
  send(data: string | Uint8Array): void {
    this.sent.push(typeof data === "string" ? JSON.parse(data) : data);
  }
  open(): void { this.readyState = FakeWebSocket.OPEN; this.onopen?.(); }
  receive(frame: object): void { this.onmessage?.({ data: JSON.stringify(frame) }); }
  close(): void { this.readyState = 3; this.onclose?.(); }
}

const sockets: SessionSocket[] = [];
beforeEach(() => {
  FakeWebSocket.instances = [];
  vi.stubGlobal("WebSocket", FakeWebSocket);
});
afterEach(() => {
  for (const socket of sockets.splice(0)) socket.close();
  vi.unstubAllGlobals();
});

function client() {
  let dims = { cols: 100, rows: 30 };
  const resized = (cols?: number, rows?: number) => {
    if (cols === undefined || rows === undefined) return;
    if (cols === dims.cols && rows === dims.rows) return;
    dims = { cols, rows };
    // xterm.resize synchronously emits onResize, including server adoption.
    socket.sendResize(cols, rows);
  };
  const socket = new SessionSocket("shell", {
    dims: () => dims,
    onReset: resized,
    onResized: resized,
    onBinary: () => {}, onTitle: () => {}, onExited: () => {}, onError: () => {},
  });
  sockets.push(socket);
  const ws = FakeWebSocket.instances.at(-1)!;
  ws.open();
  ws.receive({ type: "ready", ...dims });
  ws.sent = [];
  return { socket, ws, resized, dims: () => dims };
}

describe("terminal resize direction", () => {
  it("adopts crossed foreign resizes without reflecting stale sizes to the daemon", () => {
    const a = client();
    // Two other clients have resized before this client receives the events.
    a.ws.receive({ type: "resized", cols: 80, rows: 24 });
    a.ws.receive({ type: "resized", cols: 120, rows: 40 });
    expect(a.dims()).toEqual({ cols: 120, rows: 40 });
    expect(a.ws.sent).toEqual([]);
  });

  it("adopts a snapshot's grid without turning it into a resize request", () => {
    const a = client();
    a.ws.receive({ type: "resync", cols: 80, rows: 24 });
    expect(a.dims()).toEqual({ cols: 80, rows: 24 });
    expect(a.ws.sent).toEqual([]);
  });

  it("reconciles a fit during reconnect without echoing the snapshot's older grid", () => {
    const a = client();
    a.socket.resync();
    const reconnect = FakeWebSocket.instances.at(-1)!;
    reconnect.open();
    a.resized(110, 35);
    reconnect.sent = [];
    reconnect.receive({ type: "ready", cols: 100, rows: 30 });
    expect(a.dims()).toEqual({ cols: 100, rows: 30 });
    expect(reconnect.sent).toEqual([{ type: "resize", cols: 110, rows: 35 }]);
    reconnect.receive({ type: "resized", cols: 110, rows: 35 });
    expect(a.dims()).toEqual({ cols: 110, rows: 35 });
    expect(reconnect.sent).toHaveLength(1);
  });

  it("still sends a real fit after receiving a foreign grid", () => {
    const a = client();
    a.ws.receive({ type: "resized", cols: 80, rows: 24 });
    a.ws.sent = [];
    a.resized(110, 35);
    expect(a.ws.sent).toEqual([{ type: "resize", cols: 110, rows: 35 }]);
  });
});
