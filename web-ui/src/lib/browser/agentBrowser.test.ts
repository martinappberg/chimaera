import { get } from "svelte/store";
import { describe, expect, it, vi } from "vitest";
import type { ViewRect } from "../net/native";
import type { NativeViewEvent } from "../shared/paneViewBridge";
import {
  AgentBrowserChannel,
  OFFSCREEN_GAP_PX,
  PaneViewRegistry,
  TAKEOVER_WINDOW_MS,
  decodeBrowserLinks,
  driveProbeSaysPresent,
  hueHex,
  decodeBrowserRequest,
  offscreenRect,
  sameHello,
  shouldTakeOver,
  type AgentBrowserHost,
  type BrowserEventMsg,
  type BrowserHello,
  type BrowserReply,
  type Handover,
} from "./agentBrowser";

function request(fields: Record<string, unknown>) {
  const req = decodeBrowserRequest({ type: "browser_request", rid: 7, view: null, tab: null, ...fields });
  if (req === null) throw new Error("bad test request");
  return req;
}

/** A channel on a fake shell and socket: `calls` records shell calls,
 *  `sent` the client messages, `routes` the view-event routes. */
function harness(opts: { shared?: Set<string>; showOk?: boolean; driveResult?: unknown; driveError?: string } = {}) {
  const calls: unknown[][] = [];
  const sent: (BrowserReply | BrowserEventMsg)[] = [];
  const routes = new Map<string, (ev: NativeViewEvent) => void>();
  const handovers: Handover[] = [];
  let now = 1_000_000;
  let width = 1440;
  const shared = opts.shared ?? new Set<string>();
  const host: AgentBrowserHost = {
    native: {
      show: async (id, url, rect, hidden, agent) => {
        calls.push(["show", id, url, rect, hidden, agent]);
        return opts.showOk ?? true;
      },
      drive: async (id, action) => {
        calls.push(["drive", id, action]);
        if (opts.driveError !== undefined) throw new Error(opts.driveError);
        return opts.driveResult ?? {};
      },
      nav: async (id, action) => void calls.push(["nav", id, action]),
      close: async (id) => void calls.push(["close", id]),
      place: async (id, rect: ViewRect | null) => void calls.push(["place", id, rect]),
    },
    send: (msg) => {
      sent.push(msg);
      return true;
    },
    viewportWidth: () => width,
    handover: (h) => {
      handovers.push(h);
      return "tab-h";
    },
    shared: (tab) => shared.has(tab),
    route: (view, fn) => {
      routes.set(view, fn);
      return () => {
        if (routes.get(view) === fn) routes.delete(view);
      };
    },
    now: () => now,
    helloNow: () => void sent.push({ type: "browser_event", kind: "hello-now" } as unknown as BrowserEventMsg),
    cursorColor: (tab) => (shared.has(tab) ? "#336699" : null),
  };
  const channel = new AgentBrowserChannel(host);
  return {
    channel,
    calls,
    sent,
    routes,
    handovers,
    shared,
    advance: (ms: number) => (now += ms),
    resize: (w: number) => (width = w),
    lastReply: () => sent.filter((m): m is BrowserReply => m.type === "browser_reply").at(-1),
  };
}

describe("decodeBrowserRequest", () => {
  it("reads the envelope and keeps the op's fields", () => {
    const req = decodeBrowserRequest({ type: "browser_request", rid: 17, op: "open", view: "ab-1", tab: null, url: "/proxy/p-1/", width: 1280, height: 800 });
    expect(req).toMatchObject({ rid: 17, op: "open", view: "ab-1", tab: null });
    expect(req?.fields.url).toBe("/proxy/p-1/");
  });

  it("treats absent view/tab as null and tolerates added fields", () => {
    expect(decodeBrowserRequest({ type: "browser_request", rid: 0, op: "state", zz: 1 })).toMatchObject({ view: null, tab: null });
  });

  it("drops a frame it could not answer", () => {
    for (const bad of [
      null,
      { type: "browser_open", rid: 1, op: "open" },
      { type: "browser_request", op: "open" },
      { type: "browser_request", rid: -1, op: "open" },
      { type: "browser_request", rid: 1.5, op: "open" },
      { type: "browser_request", rid: 1, op: "" },
      { type: "browser_request", rid: 1, op: "drive", view: 3 },
      { type: "browser_request", rid: 1, op: "drive", tab: "" },
    ]) {
      expect(decodeBrowserRequest(bad)).toBeNull();
    }
  });
});

describe("decodeBrowserLinks", () => {
  it("is undefined from an older daemon and drops malformed entries", () => {
    expect(decodeBrowserLinks(undefined)).toBeUndefined();
    expect(decodeBrowserLinks([{ tab: "t1", agent_id: "s-a" }, { tab: "", agent_id: "s-a" }, { tab: "t2" }, 4])).toEqual([
      { tab: "t1", agent_id: "s-a" },
    ]);
  });
});

describe("sameHello", () => {
  const hello: BrowserHello = { type: "browser_hello", app_engine: true, visible: true, focused: false, width: 1440, height: 900, tabs: ["a", "b"] };
  it("says nothing new for an equal hello, and something for any change", () => {
    expect(sameHello(null, hello)).toBe(false);
    expect(sameHello(hello, { ...hello, tabs: ["a", "b"] })).toBe(true);
    expect(sameHello(hello, { ...hello, focused: true })).toBe(false);
    expect(sameHello(hello, { ...hello, width: 1441 })).toBe(false);
    expect(sameHello(hello, { ...hello, tabs: ["a"] })).toBe(false);
    expect(sameHello(hello, { ...hello, tabs: ["a", "c"] })).toBe(false);
  });
});

describe("PaneViewRegistry", () => {
  it("maps tabs to views both ways and lists tabs sorted", () => {
    const r = new PaneViewRegistry();
    const changes = vi.fn();
    r.onChange(changes);
    r.register("t2", "v2");
    r.register("t1", "v1");
    r.register("t1", "v1");
    expect(changes).toHaveBeenCalledTimes(2);
    expect(r.viewFor("t1")).toBe("v1");
    expect(r.tabFor("v2")).toBe("t2");
    expect(r.tabs()).toEqual(["t1", "t2"]);
  });

  it("unregisters only its own registration (a remount that came first keeps it)", () => {
    const r = new PaneViewRegistry();
    r.register("t1", "v-old");
    r.register("t1", "v-new");
    r.unregister("t1", "v-old");
    expect(r.viewFor("t1")).toBe("v-new");
    r.unregister("t1", "v-new");
    expect(r.viewFor("t1")).toBeNull();
  });

  it("counts a hand-over tab from the hand-over until its view adopts", () => {
    const r = new PaneViewRegistry();
    r.expectAdoption("t1", "ab-1");
    expect(r.viewFor("t1")).toBe("ab-1");
    expect(r.tabFor("ab-1")).toBe("t1");
    expect(r.tabs()).toEqual(["t1"]);
    r.register("t1", "ab-1");
    expect(r.pendingAdoption("t1")).toBeNull();
    expect(r.viewFor("t1")).toBe("ab-1");
    r.expectAdoption("t2", "ab-2");
    r.dropAdoption("t2");
    expect(r.tabs()).toEqual(["t1"]);
  });
});

describe("shouldTakeOver", () => {
  const now = 100_000;
  it("needs a shared pane its agent is at, not already paused", () => {
    expect(shouldTakeOver(true, { driving: true, paused: false, lastDriveAt: null }, now)).toBe(true);
    expect(shouldTakeOver(false, { driving: true, paused: false, lastDriveAt: null }, now)).toBe(false);
    expect(shouldTakeOver(true, { driving: true, paused: true, lastDriveAt: now }, now)).toBe(false);
    expect(shouldTakeOver(true, undefined, now)).toBe(false);
  });

  it("counts an agent that acted within the window, not one that left", () => {
    const acted = (ago: number) => ({ driving: false, paused: false, lastDriveAt: now - ago });
    expect(shouldTakeOver(true, acted(TAKEOVER_WINDOW_MS), now)).toBe(true);
    expect(shouldTakeOver(true, acted(TAKEOVER_WINDOW_MS + 1), now)).toBe(false);
    expect(shouldTakeOver(true, { driving: false, paused: false, lastDriveAt: null }, now)).toBe(false);
  });
});

describe("AgentBrowserChannel requests", () => {
  it("open frames the agent's page just past the window's edge, as an agent view", async () => {
    const t = harness();
    await t.channel.handle(request({ op: "open", view: "ab-1", url: "/proxy/p-1/lab", width: 1280, height: 800 }));
    expect(offscreenRect(1440, 1280, 800)).toEqual({ x: 1440 + OFFSCREEN_GAP_PX, y: 0, width: 1280, height: 800 });
    expect(t.calls).toEqual([["show", "ab-1", "/proxy/p-1/lab", { x: 1504, y: 0, width: 1280, height: 800 }, false, true]]);
    expect(t.lastReply()).toEqual({ type: "browser_reply", rid: 7, ok: true, result: {} });
  });

  it("open refuses a foreign URL or a window that can't host the view", async () => {
    const t = harness();
    await t.channel.handle(request({ op: "open", view: "ab-1", url: "https://evil.example/", width: 800, height: 600 }));
    expect(t.lastReply()).toMatchObject({ ok: false });
    await t.channel.handle(request({ op: "open", view: "ab-1", url: "//evil.example/", width: 800, height: 600 }));
    expect(t.lastReply()).toMatchObject({ ok: false });
    expect(t.calls).toEqual([]);
    const no = harness({ showOk: false });
    await no.channel.handle(request({ op: "open", view: "ab-1", url: "/proxy/p/", width: 800, height: 600 }));
    expect(no.lastReply()).toMatchObject({ ok: false, error: expect.stringContaining("can't host") });
  });

  it("drive passes the action through and answers with the shell's result or message", async () => {
    const t = harness({ driveResult: { value: 42 } });
    await t.channel.handle(request({ op: "drive", view: "ab-1", action: { type: "eval", body: "return 42" } }));
    expect(t.calls).toEqual([["drive", "ab-1", { type: "eval", body: "return 42" }]]);
    expect(t.lastReply()).toEqual({ type: "browser_reply", rid: 7, ok: true, result: { value: 42 } });
    const bad = harness({ driveError: "ReferenceError: x is not defined" });
    await bad.channel.handle(request({ op: "drive", view: "ab-1", action: { type: "eval", body: "x" } }));
    expect(bad.lastReply()).toEqual({ type: "browser_reply", rid: 7, ok: false, error: "ReferenceError: x is not defined" });
  });

  it("a request addressing a tab resolves the tab's own pane view", async () => {
    const t = harness();
    t.channel.registry.register("tab-1", "v9-abc");
    await t.channel.handle(request({ op: "drive", tab: "tab-1", action: { type: "click", x: 1, y: 2 } }));
    expect(t.calls[0]).toEqual(["drive", "v9-abc", { type: "click", x: 1, y: 2 }]);
    await t.channel.handle(request({ op: "nav", tab: "tab-1", action: "reload" }));
    expect(t.calls[1]).toEqual(["nav", "v9-abc", "reload"]);
    await t.channel.handle(request({ op: "drive", tab: "tab-gone", action: { type: "click", x: 1, y: 2 } }));
    expect(t.lastReply()).toMatchObject({ ok: false, error: expect.stringContaining("isn't open") });
  });

  it("nav accepts only history actions", async () => {
    const t = harness();
    await t.channel.handle(request({ op: "nav", view: "ab-1", action: "home" }));
    expect(t.lastReply()).toMatchObject({ ok: false });
    expect(t.calls).toEqual([]);
  });

  it("close closes an own page but never a user's pane", async () => {
    const t = harness();
    await t.channel.handle(request({ op: "open", view: "ab-1", url: "/proxy/p/", width: 800, height: 600 }));
    await t.channel.handle(request({ op: "close", view: "ab-1" }));
    expect(t.calls.at(-1)).toEqual(["close", "ab-1"]);
    expect(t.routes.has("ab-1")).toBe(false);
    t.channel.registry.register("tab-1", "v1");
    await t.channel.handle(request({ op: "close", tab: "tab-1" }));
    expect(t.lastReply()).toMatchObject({ ok: false });
    await t.channel.handle(request({ op: "close", view: "v1" }));
    expect(t.lastReply()).toMatchObject({ ok: false });
    expect(t.calls.filter((c) => c[0] === "close")).toHaveLength(1);
  });

  it("answers an unknown op with an error instead of silence", async () => {
    const t = harness();
    await t.channel.handle(request({ op: "teleport", view: "ab-1" }));
    expect(t.lastReply()).toMatchObject({ rid: 7, ok: false, error: expect.stringContaining("teleport") });
  });

  it("state sets the pane's drive state for its chrome", async () => {
    const t = harness();
    await t.channel.handle(request({ op: "state", tab: "tab-1", driving: true, paused: false }));
    expect(get(t.channel.driveState).get("tab-1")).toMatchObject({ driving: true, paused: false });
    await t.channel.handle(request({ op: "state", tab: "tab-1", driving: false }));
    expect(t.lastReply()).toMatchObject({ ok: false });
  });
});

describe("AgentBrowserChannel hand-over", () => {
  it("opens a pane adopting the live view, then hands the view over on adoption", async () => {
    const t = harness();
    await t.channel.handle(request({ op: "open", view: "ab-1", url: "/proxy/p/lab", width: 800, height: 600 }));
    await t.channel.handle(request({ op: "handover", view: "ab-1", host: "localhost", port: 8888, path: "/lab/../tree", agent: "s-a" }));
    expect(t.handovers).toEqual([{ view: "ab-1", host: "localhost", port: 8888, path: "/tree", agent: "s-a" }]);
    expect(t.lastReply()).toEqual({ type: "browser_reply", rid: 7, ok: true, result: { tab: "tab-h" } });
    // The hello listing the tab goes out before the reply.
    expect(t.sent.slice(-2).map((m) => ("kind" in m ? m.kind : m.type))).toEqual(["hello-now", "browser_reply"]);
    // The pane adopts it at the URL this window last showed it at.
    expect(t.channel.adoption("ab-1")).toEqual({ id: "ab-1", url: "/proxy/p/lab" });
    expect(t.channel.adoption("ab-unknown")).toBeNull();
    // Addressable by tab before its view mounts (behind a zoomed pane, say).
    expect(t.channel.registry.viewFor("tab-h")).toBe("ab-1");
    // Still kept off-screen until adopted…
    t.resize(1000);
    t.channel.relayout();
    expect(t.calls.at(-1)).toEqual(["place", "ab-1", { x: 1064, y: 0, width: 800, height: 600 }]);
    // …and survives a socket drop: it is the user's now.
    t.channel.socketDown();
    expect(t.calls.some((c) => c[0] === "close")).toBe(false);
    t.channel.adopted("tab-h", "ab-1");
    expect(t.routes.has("ab-1")).toBe(false);
    expect(t.channel.adoption("ab-1")).toBeNull();
    const n = t.calls.length;
    t.channel.relayout();
    expect(t.calls).toHaveLength(n);
    expect(t.channel.registry.viewFor("tab-h")).toBe("ab-1");
  });

  it("refuses a page it doesn't hold or a malformed address", async () => {
    const t = harness();
    await t.channel.handle(request({ op: "handover", view: "ab-x", host: "localhost", port: 8888, path: "/", agent: "s-a" }));
    expect(t.lastReply()).toMatchObject({ ok: false });
    await t.channel.handle(request({ op: "open", view: "ab-1", url: "/proxy/p/", width: 800, height: 600 }));
    await t.channel.handle(request({ op: "handover", view: "ab-1", host: "localhost", port: 0, path: "/", agent: "s-a" }));
    expect(t.lastReply()).toMatchObject({ ok: false });
    expect(t.handovers).toEqual([]);
  });

  it("closes the page when its tab goes before adopting it", async () => {
    const t = harness();
    await t.channel.handle(request({ op: "open", view: "ab-1", url: "/proxy/p/", width: 800, height: 600 }));
    await t.channel.handle(request({ op: "handover", view: "ab-1", host: "localhost", port: 8888, path: "/", agent: "s-a" }));
    t.channel.abandonAdoption("tab-h", "ab-1");
    expect(t.calls.at(-1)).toEqual(["close", "ab-1"]);
    expect(t.channel.registry.viewFor("tab-h")).toBeNull();
  });
});

describe("AgentBrowserChannel lifecycle", () => {
  it("socketDown closes own pages and clears every drive state", async () => {
    const t = harness();
    await t.channel.handle(request({ op: "open", view: "ab-1", url: "/proxy/p/", width: 800, height: 600 }));
    await t.channel.handle(request({ op: "state", tab: "tab-1", driving: true, paused: false }));
    t.channel.socketDown();
    expect(t.calls.at(-1)).toEqual(["close", "ab-1"]);
    expect(get(t.channel.driveState).size).toBe(0);
  });

  it("a crashed own page is reported lost and closed", async () => {
    const t = harness();
    await t.channel.handle(request({ op: "open", view: "ab-1", url: "/proxy/p/", width: 800, height: 600 }));
    t.routes.get("ab-1")?.({ id: "ab-1", kind: "crashed" });
    expect(t.sent.at(-1)).toEqual({ type: "browser_event", kind: "view_lost", view: "ab-1", tab: null });
    expect(t.calls.at(-1)).toEqual(["close", "ab-1"]);
  });
});

describe("AgentBrowserChannel take-over", () => {
  it("the person's input pauses a shared pane the agent drives, once", async () => {
    const t = harness({ shared: new Set(["tab-1"]) });
    t.channel.registry.register("tab-1", "v1");
    await t.channel.handle(request({ op: "state", tab: "tab-1", driving: true, paused: false }));
    expect(t.channel.userInput("tab-1")).toBe(true);
    expect(t.sent.at(-1)).toEqual({ type: "browser_event", kind: "takeover", view: "v1", tab: "tab-1" });
    expect(get(t.channel.driveState).get("tab-1")).toMatchObject({ paused: true });
    expect(t.channel.userInput("tab-1")).toBe(false);
  });

  it("ignores input on a pane not shared, or one its agent left alone", async () => {
    const t = harness({ shared: new Set(["tab-1"]) });
    t.channel.registry.register("tab-1", "v1");
    t.channel.registry.register("tab-2", "v2");
    await t.channel.handle(request({ op: "drive", tab: "tab-2", action: { type: "click", x: 0, y: 0 } }));
    expect(t.channel.userInput("tab-2")).toBe(false);
    await t.channel.handle(request({ op: "drive", tab: "tab-1", action: { type: "click", x: 0, y: 0 } }));
    t.advance(TAKEOVER_WINDOW_MS + 1);
    expect(t.channel.userInput("tab-1")).toBe(false);
    await t.channel.handle(request({ op: "drive", tab: "tab-1", action: { type: "click", x: 0, y: 0 } }));
    t.advance(1000);
    expect(t.channel.userInput("tab-1")).toBe(true);
  });

  it("resume hands the pane back", async () => {
    const t = harness({ shared: new Set(["tab-1"]) });
    t.channel.registry.register("tab-1", "v1");
    await t.channel.handle(request({ op: "state", tab: "tab-1", driving: false, paused: true }));
    expect(t.channel.resume("tab-1")).toBe(true);
    expect(t.sent.at(-1)).toEqual({ type: "browser_event", kind: "resume", view: "v1", tab: "tab-1" });
    expect(get(t.channel.driveState).get("tab-1")).toMatchObject({ paused: false });
  });

  it("an unlinked pane held here stops being instrumented", () => {
    const t = harness();
    t.channel.registry.register("tab-1", "v1");
    t.channel.forgetTab("tab-1");
    expect(t.calls.at(-1)).toEqual(["drive", "v1", { type: "uninstrument" }]);
    const n = t.calls.length;
    t.channel.forgetTab("tab-elsewhere");
    expect(t.calls).toHaveLength(n);
  });

  it("an unlinked pane drops its marker", async () => {
    const t = harness();
    await t.channel.handle(request({ op: "state", tab: "tab-1", driving: true, paused: false }));
    t.channel.forgetTab("tab-1");
    expect(get(t.channel.driveState).has("tab-1")).toBe(false);
  });
});

describe("agent cursor colour", () => {
  it("is the agent's link hue as #rrggbb", () => {
    expect(hueHex(0)).toBe("#d74242");
    for (const h of [180, 205, 230, 265, 320]) expect(hueHex(h)).toMatch(/^#[0-9a-f]{6}$/);
  });

  it("is added to a cursor action on a shared pane, unless one was given", async () => {
    const t = harness({ shared: new Set(["tab-1"]) });
    t.channel.registry.register("tab-1", "v1");
    await t.channel.handle(request({ op: "drive", tab: "tab-1", action: { type: "cursor", show: true, label: "Claude" } }));
    expect(t.calls.at(-1)).toEqual(["drive", "v1", { type: "cursor", show: true, label: "Claude", color: "#336699" }]);
    await t.channel.handle(request({ op: "drive", tab: "tab-1", action: { type: "cursor", show: true, color: "#000000" } }));
    expect(t.calls.at(-1)).toEqual(["drive", "v1", { type: "cursor", show: true, color: "#000000" }]);
    await t.channel.handle(request({ op: "drive", view: "ab-1", action: { type: "cursor", show: true } }));
    expect(t.calls.at(-1)).toEqual(["drive", "ab-1", { type: "cursor", show: true }]);
  });
});

describe("state ops as the daemon sends them", () => {
  it("take view null and the tab", async () => {
    const t = harness();
    const req = decodeBrowserRequest({ type: "browser_request", rid: 9, op: "state", view: null, tab: "tab-1", driving: true, paused: false })!;
    await t.channel.handle(req);
    expect(t.lastReply()).toEqual({ type: "browser_reply", rid: 9, ok: true, result: {} });
    expect(get(t.channel.driveState).get("tab-1")).toMatchObject({ driving: true, paused: false });
  });
});

describe("driveProbeSaysPresent", () => {
  it("reads the command answering as present", () => {
    expect(driveProbeSaysPresent({ ok: true })).toBe(true);
    expect(driveProbeSaysPresent({ ok: false, error: new Error("no such view") })).toBe(true);
    expect(driveProbeSaysPresent({ ok: false, error: "bad view request" })).toBe(true);
  });
  it("reads the IPC refusing the command as absent", () => {
    for (const error of [
      new Error("Command native_view_drive not found"),
      "native_view_drive not allowed. Permissions associated with this command: none",
      new Error("not in the app"),
      new Error(""),
      42,
    ]) {
      expect(driveProbeSaysPresent({ ok: false, error })).toBe(false);
    }
  });
});
