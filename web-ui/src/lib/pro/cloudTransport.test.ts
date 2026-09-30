import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cloudAction, cloudRequest, peekCatalog } from "./cloudTransport";
import { WAKE_BOUND_MS } from "./presentation";
const mocks = vi.hoisted(() => ({ api: vi.fn(), native: vi.fn(() => false), invoke: vi.fn() }));
vi.mock("../net/api", () => ({ api: mocks.api }));
vi.mock("../net/native", () => ({ isNativeShell: mocks.native, proCloudRequest: mocks.invoke }));
beforeEach(() => { mocks.api.mockReset(); mocks.native.mockReturnValue(false); mocks.invoke.mockReset(); mocks.api.mockImplementation(async () => Response.json({ available: true, providers: [] })); });
afterEach(() => vi.unstubAllGlobals());
describe("cloud provider request intent", () => {
  it("keeps catalog and attempt checks passive", async () => {
    await cloudRequest({ operation: "providers" });
    await cloudRequest({ operation: "provider_connection", connection_id: "connection-1" });
    expect(mocks.api.mock.calls.map(([path, options]) => [path, options.method, options.headers])).toEqual([
      ["/pro/cloud/providers", "GET", {}], ["/pro/cloud/connections/connection-1", "GET", {}],
    ]);
  });
  it("connects the requested catalog provider with explicit wake intent", async () => {
    await cloudRequest({ operation: "provider_connect", provider_id: "future-provider" });
    const [path, options] = mocks.api.mock.calls[0];
    expect(path).toBe("/pro/cloud/providers/future-provider/connect");
    expect(options.method).toBe("POST");
    expect(options.headers["X-Chimaera-Wake"]).toBe("interaction");
  });
  it("continues only the server-reported handoff epoch", async () => {
    await cloudRequest({ operation: "resume_handoff", workspace_id: "ws-1", expected_epoch: 7 });
    const [path, options] = mocks.api.mock.calls[0];
    expect(path).toBe("/pro/hydrate");
    expect(JSON.parse(options.body)).toEqual({ workspace_id: "ws-1", expected_epoch: 7, requires_fork: false });
  });
  it("disconnects only the named provider with explicit cloud-work acknowledgement", async () => {
    await cloudRequest({ operation: "provider_disconnect", provider_id: "future/provider", acknowledge_cloud_work: true });
    const [path, options] = mocks.api.mock.calls[0];
    expect(path).toBe("/pro/cloud/providers/future%2Fprovider/disconnect");
    expect(options.method).toBe("POST");
    expect(options.headers["X-Chimaera-Wake"]).toBe("interaction");
    expect(JSON.parse(options.body)).toEqual({ acknowledge_cloud_work: true });
  });
  it("passes the same named disconnection acknowledgement to the native bridge", async () => {
    mocks.native.mockReturnValue(true);
    mocks.invoke.mockResolvedValue({ available: true });
    const request = { operation: "provider_disconnect", provider_id: "codex", acknowledge_cloud_work: true } as const;
    await cloudRequest(request);
    expect(mocks.invoke).toHaveBeenCalledWith(request);
    expect(mocks.api).not.toHaveBeenCalled();
  });
  it("preserves only the bounded busy error for disconnection, never raw upstream output", async () => {
    const request = { operation: "provider_disconnect", provider_id: "codex", acknowledge_cloud_work: true } as const;
    mocks.api.mockResolvedValueOnce(Response.json({ error: "provider_busy" }, { status: 409 }));
    await expect(cloudRequest(request)).rejects.toThrow("provider_busy");
    mocks.api.mockResolvedValueOnce(Response.json({ error: "SECRET raw stderr" }, { status: 409 }));
    await expect(cloudRequest(request)).rejects.toThrow("This cloud operation couldn't finish. Please try again.");
  });
  it("accepts successful empty handoff responses", async () => {
    mocks.api.mockResolvedValue(new Response(null, { status: 204 }));
    await expect(cloudRequest({ operation: "resume_handoff", workspace_id: "ws-1", expected_epoch: 7 })).resolves.toEqual({});
  });
  it("opens the server-owned terminal through routing, without a URL token or hash rewrite", async () => {
    const dispatchEvent = vi.fn();
    vi.stubGlobal("window", { dispatchEvent });
    mocks.api.mockResolvedValue(Response.json({ available: true, connection: { action: { type: "terminal", workspace_id: "setup", session_id: "login" } } }));
    await cloudRequest({ operation: "open_provider_terminal", connection_id: "a" });
    const event = dispatchEvent.mock.calls[0][0];
    expect(event.type).toBe("chimaera:provider-terminal");
    expect(event.detail).toEqual({ workspaceId: "setup", sessionId: "login" });
    expect(mocks.api.mock.calls[0][1].headers).toEqual({});
  });
  it("passes native browser-open only the connection id", async () => {
    mocks.native.mockReturnValue(true);
    mocks.invoke.mockResolvedValue({ available: true });
    await cloudRequest({ operation: "open_provider_browser", connection_id: "a" });
    expect(mocks.invoke).toHaveBeenCalledWith({ operation: "open_provider_browser", connection_id: "a" });
    expect(mocks.api).not.toHaveBeenCalled();
  });
});

it("submits an authorization code only to its owned connection without navigation", async () => {
  const dispatchEvent = vi.fn();
  vi.stubGlobal("window", { dispatchEvent });
  await cloudRequest({ operation: "provider_submit", connection_id: "attempt-1", code: "synthetic-code#state" });
  const [path, options] = mocks.api.mock.calls[0];
  expect(path).toBe("/pro/cloud/connections/attempt-1/input");
  expect(options.method).toBe("POST");
  expect(JSON.parse(options.body)).toEqual({ code: "synthetic-code#state" });
  expect(dispatchEvent).not.toHaveBeenCalled();
});

it("carries only the bounded incomplete-code refusal back from a code submission", async () => {
  const request = { operation: "provider_submit", connection_id: "attempt-1", code: "synthetic-code" } as const;
  mocks.api.mockResolvedValueOnce(Response.json({ error: "authorization_code_incomplete" }, { status: 409 }));
  await expect(cloudRequest(request)).rejects.toThrow("authorization_code_incomplete");
  for (const [error, status] of [["connection_not_waiting", 409], ["SECRET raw stderr", 409], ["authorization_code_incomplete", 400]] as const) {
    mocks.api.mockResolvedValueOnce(Response.json({ error }, { status }));
    await expect(cloudRequest(request)).rejects.toThrow("This cloud operation couldn't finish. Please try again.");
  }
});

it("tells a sleeping or starting cloud machine apart from a failed request", async () => {
  for (const response of [
    () => Response.json({ error: "worker_asleep" }, { status: 503 }),
    () => Response.json({ error: "worker_unavailable" }, { status: 503 }),
    () => Response.json({ available: true }, { headers: { "X-Chimaera-Worker-State": "sleeping" } }),
    () => new Response("", { status: 503, headers: { "X-Chimaera-Worker-State": "sleeping" } }),
  ]) {
    mocks.api.mockResolvedValueOnce(response());
    await expect(cloudRequest({ operation: "providers" })).rejects.toThrow("cloud_asleep");
  }
  for (const [body, status] of [[{ error: "worker_asleep" }, 500], [{ error: "SECRET raw stderr" }, 503]] as const) {
    mocks.api.mockResolvedValueOnce(Response.json(body, { status }));
    await expect(cloudRequest({ operation: "providers" })).rejects.toThrow("This cloud operation couldn't finish. Please try again.");
  }
  mocks.api.mockResolvedValueOnce(new Response("<html>bad gateway</html>", { status: 503 }));
  await expect(cloudRequest({ operation: "info" })).rejects.toThrow("This cloud operation couldn't finish. Please try again.");
});

describe("an action the user took while the cloud comes up", () => {
  afterEach(() => vi.useRealTimers());
  const asleep = () => Response.json({ error: "worker_asleep" }, { status: 503 });
  const connect = { operation: "provider_connect", provider_id: "claude" } as const;
  it("asks again after a short pause and answers once the cloud does", async () => {
    vi.useFakeTimers();
    mocks.api.mockResolvedValueOnce(asleep()).mockResolvedValueOnce(asleep()).mockResolvedValueOnce(Response.json({ connection: { id: "c" } }));
    const result = cloudAction(connect);
    await vi.advanceTimersByTimeAsync(10_000);
    await expect(result).resolves.toEqual({ connection: { id: "c" } });
    expect(mocks.api).toHaveBeenCalledTimes(3);
    for (const [, options] of mocks.api.mock.calls) expect(options.headers["X-Chimaera-Wake"]).toBe("interaction");
  });
  it("starts no new attempt after the bound and then lets the failure stand", async () => {
    vi.useFakeTimers();
    mocks.api.mockImplementation(async () => asleep());
    const result = cloudAction(connect).catch((cause: unknown) => cause);
    await vi.advanceTimersByTimeAsync(WAKE_BOUND_MS + 10_000);
    expect(await result).toEqual(new Error("cloud_asleep"));
    const attempts = mocks.api.mock.calls.length;
    expect(attempts).toBeGreaterThan(2);
    await vi.advanceTimersByTimeAsync(60_000);
    expect(mocks.api.mock.calls.length).toBe(attempts);
  });
  it("never repeats a real failure, and stops when the press no longer matters", async () => {
    mocks.api.mockResolvedValueOnce(Response.json({ error: "SECRET" }, { status: 500 }));
    await expect(cloudAction(connect)).rejects.toThrow("This cloud operation couldn't finish. Please try again.");
    expect(mocks.api).toHaveBeenCalledTimes(1);
    vi.useFakeTimers();
    mocks.api.mockReset();
    mocks.api.mockImplementation(async () => asleep());
    let wanted = true;
    const result = cloudAction(connect, () => wanted);
    wanted = false;
    await vi.advanceTimersByTimeAsync(10_000);
    await expect(result).resolves.toBeNull();
    expect(mocks.api).toHaveBeenCalledTimes(1);
  });
});

describe("opening Agent connections", () => {
  // Showing or opening the section only looks; only Connect, Disconnect and
  // sign-in steps carry wake intent.
  const sources = import.meta.glob<string>(["/src/lib/settings/CloudSetup.svelte", "/src/lib/pro/ProviderConnections.svelte"], { query: "?raw", import: "default", eager: true });
  it("looks with one passive catalog read, in the browser and natively", async () => {
    await peekCatalog();
    expect(mocks.api.mock.calls.map(([path, options]) => [path, options.method, options.headers])).toEqual([["/pro/cloud/providers", "GET", {}]]);
    mocks.native.mockReturnValue(true);
    mocks.invoke.mockResolvedValue({ available: true, providers: [] });
    await peekCatalog();
    expect(mocks.invoke).toHaveBeenCalledWith({ operation: "providers" });
  });
  it("never sends the waking start from the connections section", () => {
    expect(Object.keys(sources)).toHaveLength(2);
    for (const [file, source] of Object.entries(sources)) {
      expect({ file, start: /operation:\s*"start"/.test(source) }).toEqual({ file, start: false });
    }
    // The panel's catalog reads all go through the passive look.
    expect(sources["/src/lib/pro/ProviderConnections.svelte"]).not.toMatch(/operation:\s*"providers"/);
    expect(sources["/src/lib/pro/ProviderConnections.svelte"]).toMatch(/peekCatalog\(/);
  });
});
