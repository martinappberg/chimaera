import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cloudRequest } from "./cloudTransport";
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
