import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
beforeEach(() => { vi.resetModules(); vi.stubGlobal("location", new URL("http://localhost/"));
  vi.stubGlobal("history", { replaceState: vi.fn() }); });
afterEach(() => { vi.unstubAllGlobals(); });
describe("captured API owner", () => {
  it("retires once per auth episode and recovers a new owner without reviving old intent", async () => {
    const api = await import("./api"); const original = api.captureApiGuard();
    const identity = get(api.apiOwner); api.clearUnauthorized(); expect(get(api.apiOwner)).toBe(identity);
    api.notifyUnauthorized(); expect(original.current()).toBe(false); expect(get(api.apiOwner)).toBeNull();
    api.notifyUnauthorized(); api.clearUnauthorized(); const fresh = api.captureApiGuard();
    expect(fresh.current()).toBe(true); expect(original.current()).toBe(false); expect(get(api.apiOwner)).not.toBe(identity);
  });
  it("an old delayed 401 cannot poison a positively recovered owner", async () => {
    const api = await import("./api"); let finish!: (response: Response) => void;
    const fetch = vi.fn(() => new Promise<Response>((resolve) => { finish = resolve; })); vi.stubGlobal("fetch", fetch);
    const old = api.api("/fixture"); await Promise.resolve();
    api.notifyUnauthorized(); api.clearUnauthorized(); const fresh = api.captureApiGuard();
    finish(new Response(null, { status: 401 })); await old;
    expect(fresh.current()).toBe(true); expect(get(api.unauthorized)).toBe(false);
  });
  it("auth replacement while placement is held refuses before the kept mutation fetch", async () => {
    vi.stubGlobal("location", new URL("https://fixture.invalid/workspace/w-one/"));
    const api = await import("./api"); let finish!: (response: Response) => void;
    const fetch = vi.fn(() => new Promise<Response>((resolve) => { finish = resolve; })); vi.stubGlobal("fetch", fetch);
    const guard = api.captureApiGuard(); const send = api.api("/pro/projects/w-one/kept/resolve", { method: "POST" }, guard);
    api.notifyUnauthorized(); api.clearUnauthorized();
    finish(Response.json({ workspace_id: "w-one", holder_id: "d-one", route_host_id: "device-d-one", epoch: 1,
      policy_revision: 1, availability: "owned", server_now: "2026-10-03T00:00:00Z", expires_at: "2026-10-03T00:01:00Z" }));
    await expect(send).rejects.toThrow(); expect(fetch).toHaveBeenCalledTimes(1);
    const placement = await import("./placement"); expect(get(placement.placementOwner)).toBeNull();
  });
});
