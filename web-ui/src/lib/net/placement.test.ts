import { describe, expect, it } from "vitest";
import { get } from "svelte/store";
import { ownerSuspended, parsePause, parsePlacement, pauseLabel, placementLabel, projectWhere, projectWhereLabel, sessionPause } from "./placement";
const live = { workspace_id: "w-one", holder_id: "d-home", route_host_id: "device-d-home", epoch: 4, policy_revision: 1, availability: "owned", server_now: "2026-09-28T19:00:00Z", expires_at: "2026-09-28T19:01:30Z" };
describe("workspace routing authority", () => {
  it("accepts exact current owner without choosing a home or worker", () => {
    expect(parsePlacement(live, "w-one").route_host_id).toBe("device-d-home");
    expect(parsePlacement({ ...live, holder_id: "worker-id", route_host_id: "worker-worker-id" }, "w-one").epoch).toBe(4);
  });
  it("rejects stale, cross-workspace and malformed authority", () => {
    for (const change of [{ workspace_id: "w-other" }, { route_host_id: "worker-other" }, { route_host_id: "prefix-device-d-home" }, { holder_id: "../x" }, { epoch: 0 }, { epoch: 2 ** 60 }, { expires_at: live.server_now }, { server_now: "bad" }, { availability: "future" }, { availability: "expired" }]) expect(() => parsePlacement({ ...live, ...change }, "w-one")).toThrow();
  });
  it("routes a sleeping cloud machine (suspended) exactly like an owner, with its expired lease", () => {
    const asleep = { ...live, holder_id: "wk", route_host_id: "worker-wk", availability: "suspended", expires_at: "2026-09-28T18:00:00Z" };
    expect(parsePlacement(asleep, "w-one")).toMatchObject({ availability: "suspended", route_host_id: "worker-wk", epoch: 4 });
    expect(parsePlacement({ ...asleep, expires_at: null }, "w-one").availability).toBe("suspended");
    for (const change of [{ route_host_id: null }, { route_host_id: "device-wk" }, { route_host_id: "worker-other" }, { holder_id: "../x" }, { epoch: 0 }, { expires_at: "soon" }])
      expect(() => parsePlacement({ ...asleep, ...change }, "w-one")).toThrow();
  });
  it("preserves unavailable states without inventing an execution route", () => {
    for (const availability of ["unowned", "expired", "privacy_disabled"])
      expect(parsePlacement({ ...live, availability, route_host_id: null }, "w-one").availability).toBe(availability);
  });
});

import { afterEach, vi } from "vitest";
import { readPlacement, sendSocketAuth, workspaceHeaders } from "./placement";
afterEach(() => { vi.unstubAllGlobals(); });
describe("passive placement transport", () => {
  it("coalesces current reads and rechecks subsequent actions without waking or acquiring", async () => {
    vi.stubGlobal("location", new URL("https://fixture.invalid/workspace/w-one/"));
    let finish!: (response: Response) => void;
    const fetch = vi.fn().mockImplementationOnce(() => new Promise<Response>(resolve => { finish = resolve; })).mockResolvedValueOnce(Response.json({ ...live, epoch: 5, holder_id: "worker-id", route_host_id: "worker-worker-id" }));
    vi.stubGlobal("fetch", fetch);
    const first=readPlacement(); const concurrent=readPlacement();
    expect(first).toBe(concurrent);
    finish(Response.json(live)); await first;
    const headers=new Headers(); await workspaceHeaders(headers);
    expect(headers.get("x-chimaera-epoch")).toBe("5");
    expect(headers.get("x-chimaera-viewer-root")).toBe("L3Byb2plY3Q");
    expect(fetch).toHaveBeenCalledTimes(2);
    for (const [url, options] of fetch.mock.calls) {
      expect(url).toBe("/workspace/w-one/placement");
      expect(options.method).toBeUndefined();
      expect(options.redirect).toBe("error");
    }
  });
  it("never authenticates a superseded socket after a delayed placement response", async () => {
    vi.stubGlobal("location", new URL("https://fixture.invalid/workspace/w-one/"));
    vi.stubGlobal("WebSocket", { OPEN: 1 });
    let finish!: (response: Response) => void;
    vi.stubGlobal("fetch", vi.fn(() => new Promise<Response>(resolve => { finish=resolve; })));
    const socket={ readyState:1,send:vi.fn(),close:vi.fn() }; let current=true; const sent=vi.fn();
    sendSocketAuth(socket as unknown as WebSocket,{type:"auth",token:"synthetic"},()=>current,sent);
    expect(socket.send).not.toHaveBeenCalled();
    current=false; finish(Response.json(live)); await readPlacement();
    expect(socket.send).not.toHaveBeenCalled(); expect(sent).not.toHaveBeenCalled();
  });
  it("queues only exact scoped auth, then signals that actions may be sent", async () => {
    vi.stubGlobal("location", new URL("https://fixture.invalid/workspace/w-one/"));
    vi.stubGlobal("WebSocket", { OPEN: 1 });
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(Response.json(live)));
    const socket={readyState:1,send:vi.fn(),close:vi.fn()};
    const sent=vi.fn(()=>expect(socket.send).toHaveBeenCalledTimes(1));
    sendSocketAuth(socket as unknown as WebSocket,{type:"auth",token:"synthetic"},()=>true,sent);
    await readPlacement();
    expect(JSON.parse(socket.send.mock.calls[0][0])).toEqual({type:"auth",token:"synthetic",workspace_id:"w-one",epoch:4,viewer_root:"L3Byb2plY3Q"});
    expect(sent).toHaveBeenCalledOnce();
  });
  it("does not retry or send auth when placement is unavailable", async () => {
    vi.stubGlobal("location", new URL("https://fixture.invalid/workspace/w-one/"));
    vi.stubGlobal("WebSocket", { OPEN: 1 });
    const fetch=vi.fn().mockResolvedValue(Response.json({error:"unavailable"},{status:503})); vi.stubGlobal("fetch",fetch);
    const socket={readyState:1,send:vi.fn(),close:vi.fn()};
    sendSocketAuth(socket as unknown as WebSocket,{type:"auth"},()=>true);
    await expect(readPlacement()).rejects.toThrow();
    expect(fetch).toHaveBeenCalledOnce(); expect(socket.send).not.toHaveBeenCalled(); expect(socket.close).toHaveBeenCalledWith(4000,"Project connection changed");
  });
});

describe("paused sessions", () => {
  it("reads the moved and paused frames and the row field alike", () => {
    expect(parsePause({ type: "moved", to: "computer" })).toEqual({ type: "moved", to: "computer" });
    expect(parsePause({ type: "moved" })).toEqual({ type: "moved", to: "elsewhere" });
    expect(parsePause({ type: "moved", to: "cloud" })).toEqual({ type: "moved", to: "cloud" });
    // Acting on another computer brought the work there: older clients read
    // `to:"computer"`, this one says which computer.
    expect(parsePause({ type: "moved", to: "computer", other: true })).toEqual({ type: "moved", to: "other" });
    expect(pauseLabel({ type: "moved", to: "other" }).status).toBe("Continuing on your other computer…");
    expect(parsePause({ type: "paused", reason: "restarting" })).toEqual({ type: "paused", reason: "restarting", provider: null });
    expect(sessionPause({ id: "s", pause: { type: "paused", reason: "needs_provider", provider: "codex" } }))
      .toEqual({ type: "paused", reason: "needs_provider", provider: "codex" });
    for (const value of [null, "moved", { type: "exited" }, { type: "paused" }]) expect(parsePause(value)).toBeNull();
    expect(sessionPause({ id: "s" })).toBeNull();
  });
  it("gives every reason its own status and only a sign-in something to do", () => {
    const reasons = ["restarting", "needs_provider", "importing", "stays_on_computer"].map((reason) =>
      pauseLabel({ type: "paused", reason, provider: "claude" }));
    const moves = (["cloud", "computer", "other"] as const).map((to) => pauseLabel({ type: "moved", to }));
    const statuses = [...reasons, ...moves].map((label) => label.status);
    expect(new Set(statuses).size).toBe(statuses.length);
    expect(reasons[1].detail).not.toBeNull();
    expect(reasons[0].detail).toBeNull();
  });
  it("names any restart by what happens next, not by an update", () => {
    const restart = pauseLabel({ type: "paused", reason: "restarting", provider: null }).status;
    expect(restart).toBe("Picking up where you left off…");
    expect(restart).not.toMatch(/update/i);
  });
  it("after sign-out, a conversation the cloud holds is not on its way anywhere", () => {
    const moving = pauseLabel({ type: "moved", to: "cloud" }).status;
    const signedOut = pauseLabel({ type: "moved", to: "cloud" }, { signedOut: true }).status;
    expect(moving).toBe("Continuing in the cloud…");
    expect(signedOut).toBe("This conversation is in the cloud. Sign in to Chimaera Pro to bring it back.");
    expect(signedOut).not.toContain("…");
    // Coming home is unaffected: it does not need the account.
    expect(pauseLabel({ type: "moved", to: "computer" }, { signedOut: true }).status).toBe("Continuing on your computer…");
  });
});

describe("unknown owner presentation", () => {
  it("never turns an opaque owner or malformed move into cloud", () => {
    for (const remote of ["opaque", "d-home", "worker", null, undefined]) {
      expect(placementLabel({ remote }, true)).toBe("Running elsewhere");
      expect(placementLabel({ remote }, false)).toBe("Running elsewhere · reconnecting");
    }
    for (const to of [undefined, null, "opaque", "worker-a"]) {
      const pause = parsePause({ type: "moved", to });
      expect(pause).toEqual({ type: "moved", to: "elsewhere" });
      expect(pauseLabel(pause).status).toBe("Continuing elsewhere…");
      expect(pauseLabel(pause, { signedOut: true }).status).toContain("Sign in to Chimaera Pro to view it.");
    }
    expect(pauseLabel(parsePause({ type: "paused", reason: "elsewhere" })).status).toBe("Continuing elsewhere…");
    expect(placementLabel({ remote: "device-d-home" }, true)).toBe("On another computer");
    expect(placementLabel({ remote: "worker-cloud" }, true)).toBe("In the cloud");
  });
});

describe("where a routed session runs", () => {
  const cloud = { remote: "worker-w1" };
  it("says asleep, not reconnecting, once the owner said it is asleep", () => {
    expect(placementLabel(cloud, false)).toBe("In the cloud · reconnecting");
    expect(placementLabel(cloud, false, { owner: "asleep" })).toBe("In the cloud · asleep");
    expect(placementLabel(cloud, true, { owner: "asleep" })).toBe("In the cloud · asleep");
    expect(placementLabel(cloud, false, { owner: "waking" })).not.toContain("reconnecting");
  });
  it("does not say reconnecting while the view's own socket is answered or kept open", () => {
    // A sleeping owner fails the roster read while its socket is kept open.
    expect(placementLabel(cloud, false, { reachable: true })).toBe("In the cloud");
    expect(placementLabel(cloud, false, { reachable: false })).toBe("In the cloud · reconnecting");
    // What the socket heard still wins.
    expect(placementLabel(cloud, false, { owner: "asleep", reachable: true })).toBe("In the cloud · asleep");
  });
  it("says reconnecting once: not in the label while the view's status line says it", () => {
    expect(placementLabel(cloud, false, { reconnectingShown: true })).toBe("In the cloud");
    expect(placementLabel("here", false, { owner: "asleep" })).toBeNull();
  });
  it("a project view names where its project runs from the latest placement read", async () => {
    vi.stubGlobal("location", new URL("https://fixture.invalid/workspace/w-one/"));
    vi.stubGlobal("fetch", vi.fn()
      .mockResolvedValueOnce(Response.json({ ...live, holder_id: "worker-id", route_host_id: "worker-worker-id" }))
      .mockResolvedValueOnce(Response.json({ ...live, holder_id: "worker-id", route_host_id: "worker-worker-id", availability: "suspended", expires_at: live.server_now }))
      .mockResolvedValueOnce(Response.json(live)));
    await readPlacement();
    expect(projectWhereLabel(get(projectWhere))).toBe("In the cloud");
    expect(ownerSuspended()).toBe(false);
    // A sleeping owner is routed (the read resolves) and named asleep.
    const asleep = await readPlacement();
    expect(asleep.route_host_id).toBe("worker-worker-id");
    expect(ownerSuspended()).toBe(true);
    expect(projectWhereLabel(get(projectWhere), { state: true })).toBe("In the cloud · asleep");
    expect(projectWhereLabel(get(projectWhere))).toBe("In the cloud");
    await readPlacement();
    expect(ownerSuspended()).toBe(false);
    expect(projectWhereLabel(get(projectWhere), { state: true })).toBe("On your computer");
    expect(projectWhereLabel(null)).toBe("This project");
  });
  it("a sleeping owner's scope is sent like an owner's: reading it never wakes it", async () => {
    vi.stubGlobal("location", new URL("https://fixture.invalid/workspace/w-one/"));
    const fetch = vi.fn().mockResolvedValue(Response.json({ ...live, holder_id: "wk", route_host_id: "worker-wk", availability: "suspended", expires_at: null }));
    vi.stubGlobal("fetch", fetch);
    const headers = new Headers();
    await workspaceHeaders(headers);
    expect(headers.get("x-chimaera-epoch")).toBe("4");
    for (const [, options] of fetch.mock.calls) expect(options.method).toBeUndefined();
  });
});
