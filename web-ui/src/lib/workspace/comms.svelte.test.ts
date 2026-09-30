import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  activateCommsWorkspace,
  commsStore,
  deliver,
  dismissWakeFailure,
  onCommsNudge,
  onCommsReconnect,
  parseComms,
  unreadFor,
  wake,
} from "./comms.svelte";

const REQ = {
  id: "w-1",
  to_sid: "s-2",
  to_name: "fix CI",
  from_sid: "s-1",
  from_name: "loader refactor",
  message: 12,
  text: "main is red",
  reason: "ask",
  created_ms: 1,
};

describe("parseComms", () => {
  it("keeps the wire shape, dropping what can't render", () => {
    const snap = parseComms({
      enabled: true,
      wakes: "auto",
      unread: { "s-1": 2, "s-2": 0, "s-3": "x", "s-4": 1.7 },
      wake_requests: [REQ, { ...REQ }, { id: "", to_sid: "s-2" }, { id: "w-2" }, null, { ...REQ, id: "w-3", reason: "hop_limit" }],
    });
    expect(snap.wakes).toBe("auto");
    expect(snap.unread).toEqual({ "s-1": 2, "s-4": 1 });
    // A repeated id would throw in a keyed list; an id-less or recipient-less
    // row has nothing to answer.
    expect(snap.wake_requests.map((r) => [r.id, r.reason])).toEqual([
      ["w-1", "ask"],
      ["w-3", "hop_limit"],
    ]);
  });

  it("defaults to on, Ask me, and nothing waiting", () => {
    expect(parseComms(null)).toEqual({ enabled: true, wakes: "ask", unread: {}, wake_requests: [] });
    expect(parseComms({ enabled: false, wakes: "sometimes" })).toMatchObject({ enabled: false, wakes: "ask" });
  });
});

describe("the comms store", () => {
  let calls: { url: string; init?: RequestInit }[] = [];
  let reply: (url: string, init?: RequestInit) => Response;
  const flush = async () => {
    for (let i = 0; i < 5; i++) await new Promise((r) => setTimeout(r, 0));
  };
  const gets = () => calls.filter((c) => (c.init?.method ?? "GET") === "GET").map((c) => c.url);

  beforeEach(() => {
    calls = [];
    reply = () =>
      new Response(JSON.stringify({ enabled: true, wakes: "ask", unread: { "s-2": 3 }, wake_requests: [REQ] }));
    vi.stubGlobal("fetch", (url: string, init?: RequestInit) => {
      calls.push({ url, init });
      return Promise.resolve(reply(url, init));
    });
  });
  afterEach(async () => {
    await flush();
    activateCommsWorkspace(null);
    vi.unstubAllGlobals();
  });

  it("loads the active workspace and refetches only when its epoch moves", async () => {
    activateCommsWorkspace("ws-a");
    await flush();
    expect(gets()).toEqual(["/api/v1/workspaces/ws-a/comms"]);
    expect(unreadFor("s-2")).toBe(3);
    expect(unreadFor("s-9")).toBe(0);
    expect(commsStore.wakeRequests.map((r) => r.id)).toEqual(["w-1"]);

    onCommsNudge({ "ws-a": 4, "ws-b": 1 });
    await flush();
    onCommsNudge({ "ws-a": 4, "ws-b": 2 }); // another workspace moved
    await flush();
    expect(gets()).toHaveLength(2);

    // A reconnect forgets the epochs: the same number is news again.
    onCommsReconnect();
    await flush();
    onCommsNudge({ "ws-a": 4 });
    await flush();
    expect(gets()).toHaveLength(4);
  });

  it("marks the route missing on an old daemon instead of showing zeros", async () => {
    reply = () => new Response(JSON.stringify({ error: "not found" }), { status: 404 });
    activateCommsWorkspace("ws-old");
    await flush();
    expect(commsStore.available).toBe(false);
    expect(commsStore.error).toBeNull();
  });

  it("answers a wake request and drops its card", async () => {
    activateCommsWorkspace("ws-w");
    await flush();
    reply = (_url, init) =>
      init?.method === "POST"
        ? new Response("{}")
        : new Response(JSON.stringify({ unread: {}, wake_requests: [] }));
    await wake("w-1", true);
    expect(commsStore.wakeRequests).toEqual([]);
    const post = calls.find((c) => c.init?.method === "POST");
    expect(post?.url).toBe("/api/v1/workspaces/ws-w/comms/wakes/w-1");
    expect(JSON.parse(String(post?.init?.body))).toEqual({ wake: true });
  });

  it("treats an already-settled wake request (404) as done, and reports a refusal", async () => {
    activateCommsWorkspace("ws-x");
    await flush();
    reply = (_url, init) =>
      init?.method === "POST"
        ? new Response(JSON.stringify({ error: "gone" }), { status: 404 })
        : new Response(JSON.stringify({ unread: {}, wake_requests: [] }));
    await expect(wake("w-1", false)).resolves.toBeUndefined();
    expect(commsStore.wakeRequests).toEqual([]);

    reply = (_url, init) =>
      init?.method === "POST"
        ? new Response(JSON.stringify({ error: "fix CI is not a live chat" }), { status: 409 })
        : new Response("{}");
    commsStore.wakeRequests = [{ ...REQ, id: "w-9" }];
    await expect(wake("w-9", true)).rejects.toThrow("fix CI is not a live chat");
    // The daemon dropped the request anyway; the lane keeps the refusal.
    expect(commsStore.wakeRequests).toEqual([]);
    expect(commsStore.wakeFailures).toEqual([{ request: { ...REQ, id: "w-9" }, message: "fix CI is not a live chat" }]);
    dismissWakeFailure("w-9");
    expect(commsStore.wakeFailures).toEqual([]);
  });

  it("hands an inbox over and clears its count", async () => {
    activateCommsWorkspace("ws-d");
    await flush();
    expect(unreadFor("s-2")).toBe(3);
    reply = (_url, init) =>
      init?.method === "POST" ? new Response("{}") : new Response(JSON.stringify({ unread: {} }));
    await deliver("s-2");
    expect(unreadFor("s-2")).toBe(0);
    const post = calls.find((c) => c.init?.method === "POST");
    expect(post?.url).toBe("/api/v1/workspaces/ws-d/comms/deliver");
    expect(JSON.parse(String(post?.init?.body))).toEqual({ session: "s-2" });
  });
});
