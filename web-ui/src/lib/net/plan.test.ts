import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ProStatus } from "./native";

const bridge = vi.hoisted(() => ({
  isNativeShell: vi.fn(),
  onProChanged: vi.fn(),
  proStatus: vi.fn(),
}));
const gateway = vi.hoisted(() => ({
  isBrowserGateway: vi.fn(),
  workbenchPath: vi.fn(() => "/app/fixture-host/"),
}));
vi.mock("./native", () => bridge);
vi.mock("./base", () => gateway);

import { paidPlan, type PaidPlan } from "./plan";

function deferred<T>(): { promise: Promise<T>; resolve: (value: T) => void } {
  let resolve!: (value: T) => void;
  return { promise: new Promise<T>((done) => { resolve = done; }), resolve };
}
function status(plan: ProStatus["plan"], signedIn = true): ProStatus {
  return { available: true, signed_in: signedIn, plan, email: null, error: null };
}
function response(plan: string | null, code = 200): Response {
  return new Response(null, {
    status: code,
    headers: plan === null ? {} : { "X-Chimaera-Plan": plan },
  });
}
const flush = async (): Promise<void> => { await vi.advanceTimersByTimeAsync(0); };

describe("shared paid plan", () => {
  let page: EventTarget & { visibilityState: "visible" | "hidden" };
  let changed: () => void;
  let unlisten: ReturnType<typeof vi.fn<() => void>>;
  let fetcher: ReturnType<typeof vi.fn>;
  let subscriptions: Array<() => void>;

  function subscribe(): PaidPlan[] {
    const values: PaidPlan[] = [];
    subscriptions.push(paidPlan.subscribe((value) => values.push(value)));
    return values;
  }
  function visibility(value: "visible" | "hidden"): void {
    page.visibilityState = value;
    page.dispatchEvent(new Event("visibilitychange"));
  }

  beforeEach(() => {
    vi.useFakeTimers();
    vi.clearAllMocks();
    subscriptions = [];
    changed = () => {};
    page = Object.assign(new EventTarget(), { visibilityState: "visible" as "visible" | "hidden" });
    vi.stubGlobal("document", page);
    unlisten = vi.fn();
    fetcher = vi.fn().mockResolvedValue(response("pro"));
    vi.stubGlobal("fetch", fetcher);
    bridge.isNativeShell.mockReturnValue(true);
    gateway.isBrowserGateway.mockReturnValue(false);
    bridge.proStatus.mockReset().mockResolvedValue(status("pro"));
    bridge.onProChanged.mockReset().mockImplementation((handler: () => void) => {
      changed = handler;
      return Promise.resolve(unlisten);
    });
  });

  afterEach(() => {
    for (const stop of subscriptions) stop();
    vi.unstubAllGlobals();
    vi.useRealTimers();
  });

  it("shares native reads, invalidates immediately, and stops only after the last subscriber", async () => {
    const first = subscribe();
    const second = subscribe();
    await flush();
    expect(first).toEqual([null, "pro"]);
    expect(second).toEqual([null, "pro"]);
    expect(bridge.proStatus).toHaveBeenCalledTimes(1);
    expect(bridge.onProChanged).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(180_000);
    expect(bridge.proStatus).toHaveBeenCalledTimes(1);
    expect(fetcher).not.toHaveBeenCalled();

    bridge.proStatus.mockResolvedValue(status("max"));
    changed();
    expect(first.at(-1)).toBeNull();
    await flush();
    expect(first.at(-1)).toBe("max");
    bridge.proStatus.mockResolvedValue(status("max", false));
    changed();
    await flush();
    expect(first.at(-1)).toBeNull();

    subscriptions.shift()!();
    expect(unlisten).not.toHaveBeenCalled();
    subscriptions.shift()!();
    expect(unlisten).toHaveBeenCalledTimes(1);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("rejects an older native response after sign-out and clears failures", async () => {
    const stale = deferred<ProStatus>();
    bridge.proStatus.mockReturnValueOnce(stale.promise);
    const values = subscribe();
    await flush();
    bridge.proStatus.mockResolvedValue(status("none", false));
    changed();
    await flush();
    stale.resolve(status("pro"));
    await flush();
    expect(values).toEqual([null]);

    bridge.proStatus.mockResolvedValue(status("pro"));
    changed();
    await flush();
    expect(values.at(-1)).toBe("pro");
    bridge.proStatus.mockRejectedValue(new Error("native unavailable"));
    changed();
    expect(values.at(-1)).toBeNull();
    await flush();
    expect(values.at(-1)).toBeNull();
  });

  it("does not let a previous subscription overwrite a new account read", async () => {
    const old = deferred<ProStatus>();
    bridge.proStatus.mockReturnValueOnce(old.promise);
    subscribe();
    await flush();
    subscriptions.shift()!();
    bridge.proStatus.mockResolvedValue(status("max"));
    const current = subscribe();
    await flush();
    old.resolve(status("pro"));
    await flush();
    expect(current).toEqual([null, "max"]);
  });

  it("disposes native registration that resolves after the last subscriber leaves", async () => {
    const registration = deferred<() => void>();
    bridge.onProChanged.mockReturnValueOnce(registration.promise);
    subscribe();
    subscriptions.shift()!();
    registration.resolve(unlisten);
    await flush();
    expect(unlisten).toHaveBeenCalledTimes(1);
    expect(bridge.proStatus).not.toHaveBeenCalled();
    visibility("visible");
    expect(bridge.proStatus).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("refreshes gateway entitlement only while visible using cookie-authenticated HEAD", async () => {
    bridge.isNativeShell.mockReturnValue(false);
    gateway.isBrowserGateway.mockReturnValue(true);
    const values = subscribe();
    await flush();
    expect(values.at(-1)).toBe("pro");
    expect(fetcher).toHaveBeenCalledWith("/app/fixture-host/", expect.objectContaining({
      method: "HEAD", credentials: "same-origin", cache: "no-store", redirect: "error",
    }));
    expect(bridge.proStatus).not.toHaveBeenCalled();
    expect(bridge.onProChanged).not.toHaveBeenCalled();
    fetcher.mockResolvedValue(response("max"));
    await vi.advanceTimersByTimeAsync(60_000);
    expect(values.at(-1)).toBe("max");
    expect(fetcher).toHaveBeenCalledTimes(2);

    visibility("hidden");
    await vi.advanceTimersByTimeAsync(180_000);
    expect(fetcher).toHaveBeenCalledTimes(2);
    fetcher.mockResolvedValue(response(null, 401));
    visibility("visible");
    await flush();
    expect(fetcher).toHaveBeenCalledTimes(3);
    expect(values.at(-1)).toBeNull();
    subscriptions.shift()!();
    expect(vi.getTimerCount()).toBe(0);
    visibility("visible");
    await flush();
    expect(fetcher).toHaveBeenCalledTimes(3);
  });

  it("times out gateway requests, rejects late responses, and treats unknown headers as unpaid", async () => {
    bridge.isNativeShell.mockReturnValue(false);
    gateway.isBrowserGateway.mockReturnValue(true);
    const hanging = deferred<Response>();
    const values = subscribe();
    await flush();
    fetcher.mockReturnValueOnce(hanging.promise);
    await vi.advanceTimersByTimeAsync(60_000);
    const signal = fetcher.mock.calls[1][1].signal as AbortSignal;
    expect(values).toEqual([null, "pro"]);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(signal.aborted).toBe(true);
    hanging.resolve(response("max"));
    await flush();
    expect(values).toEqual([null, "pro", null]);
    fetcher.mockResolvedValue(response("unknown-paid-plan"));
    await vi.advanceTimersByTimeAsync(60_000);
    expect(values).toEqual([null, "pro", null]);
    fetcher.mockResolvedValue(response("pro"));
    visibility("visible");
    await flush();
    expect(values.at(-1)).toBe("pro");
    fetcher.mockRejectedValue(new Error("connection lost"));
    visibility("visible");
    expect(values.at(-1)).toBeNull();
    await flush();
    expect(values.at(-1)).toBeNull();
  });

  it("does no account work in an ordinary browser", async () => {
    bridge.isNativeShell.mockReturnValue(false);
    const values = subscribe();
    visibility("hidden");
    visibility("visible");
    await vi.advanceTimersByTimeAsync(180_000);
    expect(values).toEqual([null]);
    expect(fetcher).not.toHaveBeenCalled();
    expect(bridge.proStatus).not.toHaveBeenCalled();
    expect(bridge.onProChanged).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(0);
  });
});
