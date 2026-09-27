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
  watching = false; session.accessChanged();
  const second = Socket.all[1]; expect(second.url).toContain("?wake=interaction");
  second.onopen?.(); session.sendInput("ok"); expect(second.sent).toHaveLength(2);
  second.onclose?.(); vi.advanceTimersByTime(601);
  expect(Socket.all[2].url).not.toContain("wake=");
  session.close();
});
