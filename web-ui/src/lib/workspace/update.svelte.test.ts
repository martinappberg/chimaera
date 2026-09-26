import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  api: vi.fn(),
  native: false,
  appUpdateStatus: vi.fn(),
}));

vi.mock("../net/api", () => ({ api: mocks.api }));
vi.mock("../net/native", () => ({
  isNativeShell: () => mocks.native,
  appUpdateStatus: mocks.appUpdateStatus,
}));

type Store = typeof import("./update.svelte");

function daemonStatus(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    current: "0.42.1",
    build: "abc1234.1790000000",
    dev: false,
    state: "current",
    available: false,
    latest: { version: "0.42.1", url: "https://example.test/v0.42.1", published_at: null },
    checked_at: 1_000,
    succeeded_at: 1_000,
    error: null,
    interval_secs: 21_600,
    ...overrides,
  };
}

function respond(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status });
}

async function load(): Promise<Store> {
  vi.resetModules();
  return import("./update.svelte");
}

describe("update awareness", () => {
  beforeEach(() => {
    mocks.api.mockReset();
    mocks.appUpdateStatus.mockReset();
    mocks.native = false;
    const storage = new Map<string, string>();
    vi.stubGlobal("localStorage", {
      getItem: (k: string) => storage.get(k) ?? null,
      setItem: (k: string, v: string) => void storage.set(k, v),
      removeItem: (k: string) => void storage.delete(k),
    });
  });

  it("parses the daemon status and refuses junk", async () => {
    const { parseUpdateStatus } = await load();
    const status = parseUpdateStatus(daemonStatus({ state: "failed", error: "Could not resolve host" }));
    expect(status?.state).toBe("failed");
    expect(status?.error).toBe("Could not resolve host");
    expect(status?.interval_secs).toBe(21_600);
    expect(parseUpdateStatus(null)).toBeNull();
    expect(parseUpdateStatus({ current: "0.42.1" })).toBeNull();
    // An unknown state word falls back to what `available`/`latest` say.
    expect(parseUpdateStatus(daemonStatus({ state: "bogus", available: true }))?.state).toBe(
      "available",
    );
  });

  it("stays silent until asked, then answers even when up to date", async () => {
    const store = await load();
    store.updateState.daemon = store.parseUpdateStatus(daemonStatus());
    expect(store.currentNotice(null)).toBeNull();

    mocks.api.mockResolvedValue(respond(daemonStatus({ checked_at: 2_000 })));
    const asking = store.checkForUpdates(true);
    expect(store.currentNotice(null)).toEqual({ kind: "checking" });
    await asking;
    expect(mocks.api.mock.calls[0][0]).toBe("/update?refresh=true");
    expect(store.currentNotice(null)).toEqual({ kind: "current", version: "0.42.1" });
    store.dismissAnswer();
    expect(store.currentNotice(null)).toBeNull();
  });

  it("never reads a failed check as up to date", async () => {
    const store = await load();
    mocks.api.mockResolvedValue(
      respond(daemonStatus({ state: "failed", error: "GitHub is rate-limiting this address" })),
    );
    await store.checkForUpdates(true);
    expect(store.currentNotice(null)).toEqual({
      kind: "failed",
      error: "GitHub is rate-limiting this address",
    });

    // The daemon itself unreachable is a failure too, not silence.
    mocks.api.mockRejectedValue(new Error("network down"));
    await store.checkForUpdates(true);
    expect(store.currentNotice(null)).toEqual({ kind: "failed", error: "network down" });
  });

  it("an explicit check outranks snooze and skip", async () => {
    const store = await load();
    const available = daemonStatus({
      state: "available",
      available: true,
      latest: { version: "0.43.0", url: "https://example.test/v0.43.0" },
    });
    store.updateState.daemon = store.parseUpdateStatus(available);
    expect(store.currentNotice(null)?.kind).toBe("release");

    store.skipUpdateVersion("0.43.0");
    expect(store.currentNotice(null)).toBeNull();

    mocks.api.mockResolvedValue(respond(available));
    await store.checkForUpdates(true);
    expect(store.currentNotice(null)).toEqual({
      kind: "release",
      version: "0.43.0",
      url: "https://example.test/v0.43.0",
    });

    // "later" on that answer puts it away again.
    store.snoozeUpdate();
    expect(store.currentNotice(null)).toBeNull();
  });

  it("the app's signed channel decides in the native shell", async () => {
    mocks.native = true;
    const store = await load();
    mocks.api.mockResolvedValue(respond(daemonStatus()));
    mocks.appUpdateStatus.mockResolvedValue({
      current: "0.42.1",
      dev: false,
      checked_at: 2_000,
      available: null,
      error: "error sending request",
      interval_secs: 21_600,
    });
    await store.checkForUpdates(true);
    expect(mocks.appUpdateStatus).toHaveBeenCalledWith(true);
    expect(store.currentNotice(null)).toEqual({ kind: "failed", error: "error sending request" });

    mocks.appUpdateStatus.mockResolvedValue({
      current: "0.42.1",
      dev: false,
      checked_at: 3_000,
      available: "0.43.0",
      error: null,
      interval_secs: 21_600,
    });
    await store.checkForUpdates(true);
    expect(store.currentNotice(null)).toEqual({ kind: "app", version: "0.43.0", url: null });
  });

  it("the app answers without waiting on a slow daemon check", async () => {
    mocks.native = true;
    const store = await load();
    let releaseDaemon: (r: Response) => void = () => {};
    mocks.api.mockReturnValue(new Promise<Response>((resolve) => (releaseDaemon = resolve)));
    mocks.appUpdateStatus.mockResolvedValue({
      current: "0.42.1",
      dev: false,
      checked_at: 2_000,
      available: null,
      error: null,
      interval_secs: 21_600,
    });
    const round = store.checkForUpdates(true);
    await vi.waitFor(() => expect(store.updateState.asked).toBe("answered"));
    expect(store.currentNotice(null)).toEqual({ kind: "current", version: "0.42.1", pending: null });

    // An announcing ask that joins the round after that early answer still
    // gets answered when the round ends.
    const joined = store.checkForUpdates(true);
    expect(store.currentNotice(null)).toEqual({ kind: "checking" });
    releaseDaemon(
      respond(
        daemonStatus({
          state: "available",
          available: true,
          latest: { version: "0.43.0", url: "https://example.test/v0.43.0" },
        }),
      ),
    );
    await round;
    await joined;
    // The daemon has seen 0.43.0 that the signed channel doesn't offer yet:
    // "newest" would be false, so the answer says so.
    expect(store.currentNotice(null)).toEqual({ kind: "current", version: "0.42.1", pending: "0.43.0" });
  });

  it("a daemon that never answers fails the check instead of hanging it", async () => {
    const store = await load();
    const timeout = new Error("signal timed out");
    timeout.name = "TimeoutError";
    mocks.api.mockRejectedValue(timeout);
    await store.checkForUpdates(true);
    expect(mocks.api.mock.calls[0][1]?.signal).toBeInstanceOf(AbortSignal);
    expect(store.currentNotice(null)).toEqual({
      kind: "failed",
      error: "the daemon did not answer in time",
    });
  });

  it("a failed re-check keeps the known app offer", async () => {
    mocks.native = true;
    const store = await load();
    const base = { current: "0.42.1", dev: false, interval_secs: 21_600 };
    store.applyAppStatus({ ...base, checked_at: 1, available: "0.43.0", error: null });
    store.applyAppStatus({ ...base, checked_at: 2, available: null, error: "timed out" });
    expect(store.updateState.appVersion).toBe("0.43.0");
    store.applyAppStatus({ ...base, checked_at: 3, available: null, error: null });
    expect(store.updateState.appVersion).toBeNull();
  });
});
