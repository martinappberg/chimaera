import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ACK_TIMEOUT_MS, type XwinHandlers, type XwinTransport } from "./crossWindow";
import {
  adoptPayload,
  canDetachOut,
  createCrossWindow,
  decodeAdopt,
  dirtyMoveReason,
  guardBlocksTab,
  removeSent,
  type CrossWindowHost,
} from "./crossWindowController";
import type { DropSpot } from "./dnd";
import { allTabs, defaultLayout, openTab, serializeLayout, soloLayout, tabKey, type Layout, type Tab } from "./layout";

// Characterization of the window's cross-window moves as App.svelte ran them
// before the extraction: the dirty-move guards, the adopt payload, the
// receiver's refusals, and the sender-removes-only-on-ok-ack policy driven
// through a fake transport.

const file = (path: string): Tab => ({ surface: "file", path });
const term = (sessionId: string): Tab => ({ surface: "terminal", sessionId });
const none = new Set<string>();

describe("pure guards", () => {
  it("canDetachOut needs a workspace and refuses a job window", () => {
    expect(canDetachOut("w-1", false)).toBe(true);
    expect(canDetachOut(null, false)).toBe(false);
    expect(canDetachOut("w-1", true)).toBe(false);
  });

  it("a dirty file blocks with the save message; the file check wins over a draft", () => {
    expect(dirtyMoveReason([file("/a")], new Set(["/a"]), none)).toBe(
      "save the file before moving it to another window",
    );
    expect(dirtyMoveReason([term("s1"), file("/a")], new Set(["/a"]), new Set(["s1"]))).toBe(
      "save the file before moving it to another window",
    );
  });

  it("a volatile chat draft blocks its terminal tab with the draft message", () => {
    expect(dirtyMoveReason([term("s1")], none, new Set(["s1"]))).toBe(
      "send or clear the chat draft before moving it to another window",
    );
  });

  it("the guard keys by surface: a dirty path never blocks a terminal, a draft id never a file", () => {
    expect(dirtyMoveReason([term("/a")], new Set(["/a"]), none)).toBeNull();
    expect(dirtyMoveReason([file("s1")], none, new Set(["s1"]))).toBeNull();
    expect(dirtyMoveReason([file("/b")], new Set(["/a"]), none)).toBeNull();
  });

  it("guardBlocksTab agrees with the guard per tab", () => {
    expect(guardBlocksTab(file("/a"), new Set(["/a"]), none)).toBe(true);
    expect(guardBlocksTab(term("s1"), none, new Set(["s1"]))).toBe(true);
    expect(guardBlocksTab(file("/a"), none, new Set(["/a"]))).toBe(false);
    expect(guardBlocksTab({ surface: "dashboard" } as Tab, new Set(["/a"]), new Set(["s1"]))).toBe(false);
  });
});

describe("adopt payload and decode", () => {
  it("the payload is { ws, layout } with layout a serialized solo blob", () => {
    const tabs = [file("/a"), file("/b")];
    const p = adoptPayload("w-1", tabs, 1, 15) as { ws: unknown; layout: unknown };
    expect(Object.keys(p)).toEqual(["ws", "layout"]);
    expect(p.ws).toBe("w-1");
    const solo = soloLayout(tabs, 1, 15);
    const want = serializeLayout(solo) as { root: { id: string } };
    const got = p.layout as { root: { id: string } };
    // Pane ids are minted per call; everything else matches.
    expect({ ...got, root: { ...got.root, id: "x" }, focused: "x" }).toEqual({
      ...want,
      root: { ...want.root, id: "x" },
      focused: "x",
    });
  });

  it("decode round-trips the tabs and names the sender's active tab", () => {
    const tabs = [file("/a"), file("/b")];
    const got = decodeAdopt(adoptPayload("w-1", tabs, 1), "w-1", true);
    expect(got?.tabs.map(tabKey)).toEqual(tabs.map(tabKey));
    expect(got?.activeKey).toBe(tabKey(file("/b")));
  });

  it("refuses while the layout is not ready, for another workspace, malformed or empty", () => {
    const ok = adoptPayload("w-1", [file("/a")], 0);
    expect(decodeAdopt(ok, "w-1", false)).toBeNull();
    expect(decodeAdopt(ok, "w-2", true)).toBeNull();
    expect(decodeAdopt(null, "w-1", true)).toBeNull();
    expect(decodeAdopt("nope", "w-1", true)).toBeNull();
    expect(decodeAdopt({ ws: "w-1", layout: { junk: 1 } }, "w-1", true)).toBeNull();
    expect(decodeAdopt(adoptPayload("w-1", [], 0), "w-1", true)).toBeNull();
  });

  it("removeSent drops each sent tab wherever it now sits and ignores absent ones", () => {
    let l = defaultLayout();
    l = openTab(l, file("/a"));
    l = openTab(l, file("/b"));
    const next = removeSent(l, [file("/a"), file("/zzz")]);
    expect(allTabs(next).map(tabKey)).toEqual([tabKey(file("/b"))]);
  });
});

// --- the controller over a fake transport -----------------------------------

interface Fake extends XwinTransport {
  handlers: XwinHandlers | null;
  ackHandler: ((transfer: number, ok: boolean) => void) | null;
  acked: [number, boolean][];
  sent: { winId: string; payload: unknown; transfer: number }[];
  closed: boolean;
}

function fakeTransport(): Fake {
  const f: Fake = {
    handlers: null,
    ackHandler: null,
    acked: [],
    sent: [],
    closed: false,
    track: async () => false,
    drop: async () => ({ routed: false }),
    cancel: () => {},
    roster: async () => [{ winId: "win-b", label: "B", detached: false }],
    adoptInto: async (winId, payload, transfer) => {
      f.sent.push({ winId, payload, transfer });
    },
    ack: (transfer, ok) => f.acked.push([transfer, ok]),
    incoming: (h) => {
      f.handlers = h;
      return () => (f.handlers = null);
    },
    acks: (h) => {
      f.ackHandler = h;
      return () => (f.ackHandler = null);
    },
    close: () => (f.closed = true),
  };
  return f;
}

function harness(init: Layout, opts: { ready?: boolean; dirty?: Set<string> } = {}) {
  const state = {
    layout: init,
    ws: "w-1" as string | null,
    ready: opts.ready ?? true,
    dropSpot: null as DropSpot | null,
    flashes: [] as string[],
    adopted: 0,
  };
  const transports: Fake[] = [];
  const host: CrossWindowHost = {
    winKey: () => "win-a",
    getLayout: () => state.layout,
    setLayout: (l) => (state.layout = l),
    getWs: () => state.ws,
    getWsRoot: () => null,
    isReady: () => state.ready,
    isJobWindow: () => false,
    getOrigin: () => null,
    windowLabel: () => "A",
    isDetached: () => false,
    getDropSpot: () => state.dropSpot,
    setDropSpot: (s) => (state.dropSpot = s),
    dirtyFiles: () => opts.dirty ?? none,
    volatileDrafts: () => none,
    flash: (m) => state.flashes.push(m),
    onAdopted: () => state.adopted++,
    createTransport: (ws) => {
      if (ws === null) return null;
      const t = fakeTransport();
      transports.push(t);
      return t;
    },
    isNative: () => false,
  };
  return { state, transports, xw: createCrossWindow(host) };
}

function twoTabs(): Layout {
  return openTab(openTab(defaultLayout(), file("/a")), file("/b"));
}

/** Let the menu path's roster probe and adoptInto settle. */
async function settle(): Promise<void> {
  for (let i = 0; i < 5; i++) await Promise.resolve();
}

describe("createCrossWindow", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("sender removes the moved tab only on an ok ack", async () => {
    const { state, transports, xw } = harness(twoTabs());
    xw.subscribe("w-1");
    const pane = state.layout.focusedPaneId;
    xw.moveTabToWindow(pane, 0, "win-b");
    await settle();
    const [t] = transports;
    expect(t.sent).toHaveLength(1);
    expect(t.sent[0].winId).toBe("win-b");
    expect(allTabs(state.layout)).toHaveLength(2); // still here before the ack
    t.ackHandler!(t.sent[0].transfer, true);
    expect(allTabs(state.layout).map(tabKey)).toEqual([tabKey(file("/b"))]);
  });

  it("a refused ack leaves the tabs and flashes the decline", async () => {
    const { state, transports, xw } = harness(twoTabs());
    xw.subscribe("w-1");
    xw.moveTabToWindow(state.layout.focusedPaneId, 0, "win-b");
    await settle();
    const [t] = transports;
    t.ackHandler!(t.sent[0].transfer, false);
    expect(allTabs(state.layout)).toHaveLength(2);
    expect(state.flashes).toEqual(["the other window declined the move"]);
  });

  it("a lost ack leaves the tabs and flashes once at the soft deadline", async () => {
    const { state, xw } = harness(twoTabs());
    xw.subscribe("w-1");
    xw.moveTabToWindow(state.layout.focusedPaneId, 0, "win-b");
    await settle();
    vi.advanceTimersByTime(ACK_TIMEOUT_MS + 60);
    expect(allTabs(state.layout)).toHaveLength(2);
    expect(state.flashes).toEqual(["the move didn't complete — the tab stays here"]);
  });

  it("an ack for a move started in another workspace touches nothing", async () => {
    const { state, transports, xw } = harness(twoTabs());
    xw.subscribe("w-1");
    xw.moveTabToWindow(state.layout.focusedPaneId, 0, "win-b");
    await settle();
    state.ws = "w-2";
    transports[0].ackHandler!(transports[0].sent[0].transfer, true);
    expect(allTabs(state.layout)).toHaveLength(2);
  });

  it("a dirty tab never leaves: no send, the reason flashes", async () => {
    const { state, transports, xw } = harness(twoTabs(), { dirty: new Set(["/a"]) });
    xw.subscribe("w-1");
    xw.moveTabToWindow(state.layout.focusedPaneId, 0, "win-b");
    await settle();
    expect(transports[0].sent).toHaveLength(0);
    expect(state.flashes).toEqual(["save the file before moving it to another window"]);
  });

  it("the receiver adopts and acks ok when ready", () => {
    const { state, transports, xw } = harness(defaultLayout());
    xw.subscribe("w-1");
    state.dropSpot = { kind: "edge", edge: "left" };
    transports[0].handlers!.onDrop({ payload: adoptPayload("w-1", [file("/x")], 0), transfer: 7 });
    expect(transports[0].acked).toEqual([[7, true]]);
    expect(allTabs(state.layout).map(tabKey)).toEqual([tabKey(file("/x"))]);
    expect(state.dropSpot).toBeNull();
    expect(state.adopted).toBe(1);
  });

  it("the receiver refuses (acks false, layout untouched) while its layout is restoring", () => {
    const { state, transports, xw } = harness(defaultLayout(), { ready: false });
    xw.subscribe("w-1");
    const before = state.layout;
    transports[0].handlers!.onDrop({ payload: adoptPayload("w-1", [file("/x")], 0), transfer: 9 });
    expect(transports[0].acked).toEqual([[9, false]]);
    expect(state.layout).toBe(before);
    expect(state.adopted).toBe(0);
  });

  it("re-subscribes per workspace, and teardown closes the transport and clears the drop spot", () => {
    const { state, transports, xw } = harness(defaultLayout());
    const off1 = xw.subscribe("w-1");
    state.dropSpot = { kind: "edge", edge: "right" };
    off1?.();
    expect(transports[0].closed).toBe(true);
    expect(transports[0].handlers).toBeNull();
    expect(state.dropSpot).toBeNull();
    xw.subscribe("w-2");
    expect(transports).toHaveLength(2);
    expect(xw.subscribe(null)).toBeUndefined();
  });

  it("the move menu's roster is the last refresh", async () => {
    const { xw } = harness(defaultLayout());
    expect(xw.moveTargets()).toEqual([]);
    xw.subscribe("w-1");
    await settle();
    expect(xw.moveTargets().map((w) => w.winId)).toEqual(["win-b"]);
  });
});
