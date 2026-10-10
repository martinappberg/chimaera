import { describe, expect, it, vi, beforeEach } from "vitest";
const bridge = vi.hoisted(() => ({ onProReturn: vi.fn(), proTakeReturn: vi.fn() }));
vi.mock("./native", () => bridge);
import { listenForProReturn } from "./proReturn";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}
const flush = async () => { for (let i = 0; i < 6; i += 1) await Promise.resolve(); };

describe("targeted native account return", () => {
  let event: () => void;
  beforeEach(() => {
    vi.resetAllMocks();
    bridge.proTakeReturn.mockResolvedValue(false);
    bridge.onProReturn.mockImplementation((handler: () => void) => { event = handler; return Promise.resolve(vi.fn()); });
  });
  it("registers before consuming a pending return, then ignores unrelated events", async () => {
    const registration = deferred<() => void>();
    bridge.onProReturn.mockImplementation((handler: () => void) => { event = handler; return registration.promise; });
    bridge.proTakeReturn.mockResolvedValueOnce(true).mockResolvedValue(false);
    const open = vi.fn(); const unlisten = vi.fn();
    const stop = listenForProReturn(open);
    expect(bridge.proTakeReturn).not.toHaveBeenCalled();
    registration.resolve(unlisten); await flush();
    expect(open).toHaveBeenCalledTimes(1);
    event(); await flush(); expect(open).toHaveBeenCalledTimes(1);
    stop(); expect(unlisten).toHaveBeenCalledTimes(1);
  });
  it("coalesces events during a consume without losing a later pending return", async () => {
    const first = deferred<boolean>();
    bridge.proTakeReturn.mockReturnValueOnce(first.promise).mockResolvedValueOnce(true);
    const open = vi.fn(); const stop = listenForProReturn(open); await flush();
    event(); event(); expect(bridge.proTakeReturn).toHaveBeenCalledTimes(1);
    first.resolve(false); await flush();
    expect(bridge.proTakeReturn).toHaveBeenCalledTimes(2);
    expect(open).toHaveBeenCalledTimes(1); stop();
  });
  it("never navigates an unmounted view from a late consume or registration", async () => {
    const response = deferred<boolean>(); bridge.proTakeReturn.mockReturnValue(response.promise);
    const open = vi.fn(); const stop = listenForProReturn(open); await flush();
    stop(); response.resolve(true); await flush(); expect(open).not.toHaveBeenCalled();
    const registration = deferred<() => void>(); const unlisten = vi.fn();
    bridge.onProReturn.mockReturnValue(registration.promise); bridge.proTakeReturn.mockClear();
    const stopEarly = listenForProReturn(open); stopEarly(); registration.resolve(unlisten); await flush();
    expect(unlisten).toHaveBeenCalledTimes(1); expect(bridge.proTakeReturn).not.toHaveBeenCalled();
  });
  it("tolerates an older shell with no return command", async () => {
    bridge.proTakeReturn.mockRejectedValue(new Error("unknown command"));
    const open = vi.fn(); const stop = listenForProReturn(open); await flush();
    expect(open).not.toHaveBeenCalled(); stop();
  });
});
