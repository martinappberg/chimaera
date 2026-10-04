import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { legacyCloudRequest as cloudRequest } from "../extensions/accountDaemon";
import type { CloudSetupRequest } from "../net/native";
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
  it("never opens a window on the cloud: an older page's terminal request reaches nothing", async () => {
    const dispatchEvent = vi.fn();
    vi.stubGlobal("window", { dispatchEvent });
    const older = { operation: "open_provider_terminal", connection_id: "a" } as unknown as CloudSetupRequest;
    await expect(cloudRequest(older)).rejects.toThrow("This cloud operation isn't available.");
    expect(mocks.api).not.toHaveBeenCalled();
    expect(dispatchEvent).not.toHaveBeenCalled();
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
